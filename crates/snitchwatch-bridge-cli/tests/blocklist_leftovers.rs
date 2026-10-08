//! Issue #73: blocklist rules Snitchwatch made that nothing manages are
//! counted for the Blocklists page and removed when the user asks, through
//! the mock daemon, over the Unix socket of a system service (see
//! `src/leftover_unix_tests.rs`). Over the legacy TCP connection, where any
//! local process can pose as the daemon, nothing is offered or deleted: these
//! tests pin that, and that a service managing its rules reports none.

use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Notification, Operator, Rule};
use tokio::sync::{broadcast, mpsc};

const WAIT: Duration = Duration::from_secs(10);
const ROOT: &str = "/var/lib/snitchwatch/blocklists";

fn bridge_rule(list: &str, kind: &str) -> Rule {
    Rule {
        name: format!("z00-blocklist:{list}:{kind}"),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "lists".into(),
            operand: format!("lists.{kind}"),
            data: format!("{ROOT}/{list}/{kind}"),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Someone else's rule under the blocklist prefix: not the bridge's shape.
fn foreign_rule() -> Rule {
    Rule {
        name: "z00-blocklist:foreign:domains".into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "x.example".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Setup {
    _sockets: tempfile::TempDir,
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
    _daemon: MockOpensnitchd,
    seen: mpsc::Receiver<Notification>,
}

/// A bridge with `storage` and a daemon whose rule list is `snapshot`.
async fn start(storage: Storage, mode: BridgeMode, snapshot: Vec<Rule>) -> Setup {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage,
            blocklist_fetcher: None,
            mode,
        },
    )
    .await
    .unwrap();
    let rx = bridge.broadcast_tx.subscribe();
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon
        .subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules: snapshot,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    Setup {
        _sockets: sockets,
        bridge,
        rx,
        _daemon: daemon,
        seen,
    }
}

/// The next leftover count the bridge announces.
async fn next_count(rx: &mut broadcast::Receiver<ServerMessage>) -> u32 {
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetBlocklistLeftovers { count, .. } = rx.recv().await.unwrap() {
                return count;
            }
        }
    })
    .await
    .expect("no SetBlocklistLeftovers")
}

fn snapshot() -> Vec<Rule> {
    vec![
        bridge_rule("ads-0123456789abcdef", "domains"),
        bridge_rule("ads-0123456789abcdef", "ips"),
        foreign_rule(),
    ]
}

/// Over TCP the snapshot could be a local process's, and a delete goes to
/// every stream: nothing is listed, and a request to remove sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_the_legacy_tcp_connection_nothing_is_offered_or_deleted() {
    let state = tempfile::tempdir().unwrap();
    for (storage, mode) in [
        (
            Storage::Ephemeral(EphemeralReason::NotConfigured),
            BridgeMode::System,
        ),
        (
            Storage::Persistent(state.path().canonicalize().unwrap()),
            BridgeMode::User,
        ),
    ] {
        let mut s = start(storage, mode, snapshot()).await;
        s.bridge
            .inbound_tx
            .send(ClientMessage::RemoveLeftoverBlocklistRules)
            .await
            .unwrap();
        s.bridge
            .inbound_tx
            .send(ClientMessage::RequestSnapshot)
            .await
            .unwrap();
        assert_eq!(next_count(&mut s.rx).await, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), s.seen.recv())
                .await
                .is_err(),
            "a delete reached the daemon"
        );
        s.bridge.shutdown();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_that_manages_its_rules_reports_none_and_ignores_the_request() {
    let state = tempfile::tempdir().unwrap();
    let mut s = start(
        Storage::Persistent(state.path().canonicalize().unwrap()),
        BridgeMode::System,
        Vec::new(),
    )
    .await;
    s.bridge
        .inbound_tx
        .send(ClientMessage::RemoveLeftoverBlocklistRules)
        .await
        .unwrap();
    s.bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    assert_eq!(next_count(&mut s.rx).await, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), s.seen.recv())
            .await
            .is_err(),
        "nothing was sent to the daemon"
    );
    s.bridge.shutdown();
}
