//! Prompt-slot plan Part C, through `bridge_feed::dispatch_to`: "Decide
//! later" reaches only a live session that advertised it, with the row id's
//! session prefix stripped like a verdict's, and a reconnect never inherits
//! the capability. (Kept out of `tests.rs`, which is near the 800-line cap.)

use super::*;

fn handles_and_queue() -> (
    BridgeHandles,
    Arc<Mutex<ConnectionState>>,
    mpsc::Receiver<QueuedClientMessage>,
) {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, inbound_rx) = mpsc::channel(8);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };
    (handles, connection, inbound_rx)
}

fn decide_later(row_id: &str) -> ClientMessage {
    ClientMessage::DecideLater {
        row_id: row_id.to_string(),
    }
}

#[tokio::test]
async fn decide_later_reaches_only_a_session_that_takes_it() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();

    // An older bridge: nothing is queued.
    let old = mark_connected(&connection, true);
    assert!(!handles.advertises_decide_later(old));
    assert!(!crate::bridge_feed::decide_later_for_row(
        Some(&handles),
        "1:7"
    ));
    assert!(crate::bridge_feed::dispatch_to(&handles, decide_later("1:7"), false).is_err());
    assert!(inbound_rx.try_recv().is_err());

    // A bridge that advertised it gets the wire id, for its own session.
    disconnect_and_discard(&connection, &mut inbound_rx);
    let current = mark_connected(&connection, true);
    mark_decide_later(&connection, current);
    assert!(crate::bridge_feed::decide_later_for_row(
        Some(&handles),
        "2:7"
    ));
    crate::bridge_feed::dispatch_to(&handles, decide_later("2:7"), false).unwrap();
    let queued = inbound_rx.try_recv().expect("queued");
    assert_eq!(queued.connection_id, current);
    assert_eq!(queued.message, decide_later("7"));

    // A row from the previous session, or a malformed id, is refused.
    assert!(crate::bridge_feed::dispatch_to(&handles, decide_later("1:7"), false).is_err());
    assert!(crate::bridge_feed::dispatch_to(&handles, decide_later("7"), false).is_err());
    assert!(!crate::bridge_feed::decide_later_for_row(None, "2:7"));
    assert!(inbound_rx.try_recv().is_err());

    // A reconnect starts without it, and a stale mark can't set it.
    disconnect_and_discard(&connection, &mut inbound_rx);
    assert!(!handles.advertises_decide_later(current));
    let next = mark_connected(&connection, true);
    mark_decide_later(&connection, current);
    assert!(!handles.advertises_decide_later(next));
}
