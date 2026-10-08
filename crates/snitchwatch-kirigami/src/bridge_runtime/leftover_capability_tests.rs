//! Issue #73 in the client runtime: a bridge that doesn't say it reports
//! leftover blocklist rules never says "none left", so the count an earlier
//! bridge sent is cleared when connecting to it.

use super::*;

fn received(rx: &mut broadcast::Receiver<ReceivedServerMessage>) -> Option<ReceivedServerMessage> {
    rx.try_recv().ok()
}

#[test]
fn a_bridge_without_the_capability_clears_an_earlier_leftover_count() {
    let (tx, mut rx) = broadcast::channel(4);
    clear_unreported_leftovers(&[], 7, &tx);
    let message = received(&mut rx).expect("the count is cleared");
    assert_eq!(message.connection_id, 7);
    assert_eq!(
        message.message,
        ServerMessage::SetBlocklistLeftovers {
            count: 0,
            cause: None,
            reason: None
        }
    );
}

#[test]
fn a_bridge_with_it_is_believed() {
    let (tx, mut rx) = broadcast::channel(4);
    let capabilities = vec![
        "ruleHits".to_string(),
        snitchwatch_bridge::bridge_capabilities::BLOCKLIST_LEFTOVERS.to_string(),
    ];
    clear_unreported_leftovers(&capabilities, 7, &tx);
    assert!(received(&mut rx).is_none());
}

// --- Through the relay, against a bridge that does or doesn't advertise it ----

use super::tests::{accept_with_ack, pause_channel, wait_for_session};

/// Run `client_loop` against a fake bridge that acknowledges with `ack`;
/// returns what the shell feed received, once the session is live.
async fn feed_after_connecting(ack: &str) -> Vec<ReceivedServerMessage> {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("bridge.sock");
    let token = snitchwatch_bridge::auth::Token::generate();
    snitchwatch_bridge::auth::write_token_file(&token, &dir.path().join("token")).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let ack = ack.to_string();
    let server = tokio::spawn(async move {
        let ws = accept_with_ack(&listener, &token, &ack).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(ws);
    });

    let (broadcast_tx, mut feed) = broadcast::channel(16);
    let (_inbound_tx, inbound_rx) = mpsc::channel(1);
    let (tray_tx, _) = watch::channel(ReceivedTrayState {
        connection_id: 0,
        state: BridgeTrayState::Idle,
    });
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let client = tokio::spawn(client_loop(
        socket_path,
        broadcast_tx,
        inbound_rx,
        Arc::new(Mutex::new(LinkStatus::default())),
        ShellFeeds {
            tray_tx,
            notice_tx: broadcast::channel(1).0,
            pause_tx: pause_channel().0,
            slot_tx: watch::channel(ReceivedPromptSlot::default()).0,
        },
        connection.clone(),
    ));
    wait_for_session(&connection, 1, true).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut seen = Vec::new();
    while let Ok(message) = feed.try_recv() {
        seen.push(message);
    }
    client.abort();
    server.await.unwrap();
    seen
}

fn leftovers_cleared(seen: &[ReceivedServerMessage]) -> bool {
    seen.iter().any(|m| {
        m.connection_id == 1
            && m.message
                == ServerMessage::SetBlocklistLeftovers {
                    count: 0,
                    cause: None,
                    reason: None,
                }
    })
}

#[tokio::test]
async fn a_bare_acknowledgement_makes_the_gui_forget_an_earlier_leftover_count() {
    let seen = feed_after_connecting(r#"{"action":"authenticated"}"#).await;
    assert!(leftovers_cleared(&seen), "{seen:?}");
}

#[tokio::test]
async fn an_acknowledgement_that_lists_the_capability_leaves_the_count_alone() {
    let ack = serde_json::to_string(&ServerMessage::Authenticated {
        capabilities: snitchwatch_bridge::bridge_capabilities::advertised(),
    })
    .unwrap();
    let seen = feed_after_connecting(&ack).await;
    assert!(!leftovers_cleared(&seen), "{seen:?}");
}
