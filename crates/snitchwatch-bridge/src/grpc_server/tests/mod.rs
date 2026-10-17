use super::*;
use crate::cache::connections::Verdict;
use crate::cache::rules::RulesCache;
use crate::daemon_commands::CommandError;
use crate::ws_messages::{VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::ui_client::UiClient;
use snitchwatch_proto::protocol::{NotificationReplyCode, Operator};
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tonic::transport::Server;

// DaemonLiveness/StreamGuard's own unit tests now live in
// `daemon_liveness.rs`, next to the type they test.

/// Drains the channel: nothing but `PromptSlot` messages are left, and the
/// last says the slot is free (a trailing held one would be a stale prompt).
fn assert_only_a_free_slot_is_left(rx: &mut broadcast::Receiver<ServerMessage>) {
    let left: Vec<ServerMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        left.iter()
            .all(|m| matches!(m, ServerMessage::PromptSlot { .. })),
        "{left:?}"
    );
    assert!(
        matches!(
            left.last(),
            Some(ServerMessage::PromptSlot {
                holder: None,
                holders: 0,
                ..
            })
        ),
        "{left:?}"
    );
}

/// The next queued broadcast that isn't a `PromptSlot` (those are covered by
/// `prompt_slot_tests.rs`).
fn try_recv_skipping_slot(
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> Result<ServerMessage, broadcast::error::TryRecvError> {
    loop {
        match rx.try_recv() {
            Ok(ServerMessage::PromptSlot { .. }) => continue,
            other => return other,
        }
    }
}

async fn spawn_test_service() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache,
        tx,
        tray_pub,
        notice_bus,
        Arc::new(FilterPause::new()),
    )
    .into_server();

    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

// Lifecycle fixtures keep internal subscribers separate from GUI leases.
fn lifecycle_service() -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
) {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, rx) = broadcast::channel(64);
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    (svc, cache, rx)
}

async fn lifecycle_pending(rx: &mut broadcast::Receiver<ServerMessage>) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() {
                return rows[0].id.clone();
            }
        }
    })
    .await
    .unwrap()
}

async fn lifecycle_removed(rx: &mut broadcast::Receiver<ServerMessage>, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match rx.recv().await.unwrap() {
                ServerMessage::RemoveConnectionRows { ids }
                    if ids.iter().any(|removed| removed == id) =>
                {
                    return
                }
                ServerMessage::UpdateRules { .. } | ServerMessage::UpdateConnectionRows { .. } => {
                    panic!("cancelled Ask produced a verdict side effect")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

fn rules_service(
    transport: DaemonTransport,
) -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
) {
    let (svc, cache, rx) = lifecycle_service();
    (svc.with_daemon_transport(transport), cache, rx)
}

fn daemon_reply(id: u64, code: NotificationReplyCode, data: &str) -> NotificationReply {
    NotificationReply {
        id,
        code: code as i32,
        data: data.to_string(),
    }
}

fn hello() -> NotificationReply {
    daemon_reply(0, NotificationReplyCode::Ok, "")
}

fn daemon_rule(name: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn with_rules(rules: Vec<Rule>) -> ClientConfig {
    ClientConfig {
        rules,
        ..Default::default()
    }
}

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn set_rules_names(msg: ServerMessage) -> Vec<String> {
    match msg {
        ServerMessage::SetRules { rules } => rules
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect(),
        other => panic!("expected SetRules, got {other:?}"),
    }
}

fn cached(svc: &UiService) -> RulesCache {
    svc.rules_handle().lock().unwrap().clone()
}

mod alerts_lifecycle;
mod ask_rule_verdicts;
mod daemon_redial;
mod pause;
mod ping_subscribe;
mod recent_block;
mod rules_snapshot;
