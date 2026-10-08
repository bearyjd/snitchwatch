//! P2.6 Part 1 (owner decision N1): per-rule hit counts, derived from the
//! daemon's ping statistics, persist in the resolved state directory
//! (`rule_hits.json`) and reach the GUI as `RuleHits`. Driven through
//! `run_with_options` and the mock daemon, like `blocklist_wiring.rs`:
//! - the snapshot answer carries `RuleHits` even before the first ping;
//! - counts follow pings, are shown only for rules in the daemon's snapshot,
//!   and survive a restart (file mode 0600), with `lossy` set because the
//!   bridge was down;
//! - rules missing from the first snapshot after a restart are dropped for
//!   good;
//! - the ticker broadcasts changes;
//! - without a usable state directory, or with an unreadable file, the
//!   counts stay in memory and the message says why.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage, StorageStatus};
use snitchwatch_bridge_cli::{
    run, run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge,
    Storage,
};
use snitchwatch_proto::protocol::{ClientConfig, Event, Operator, Rule, Statistics};
use tokio::sync::{broadcast, mpsc};

const WAIT: Duration = Duration::from_secs(12);

fn config(dir: &Path) -> BridgeConfig {
    BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.join("bridge.sock"),
        cache_capacity: 64,
    }
}

async fn start(storage: Storage, sockets: &Path) -> RunningBridge {
    run_with_options(
        config(sockets),
        RunOptions {
            storage,
            blocklist_fetcher: None,
            mode: BridgeMode::User,
        },
    )
    .await
    .unwrap()
}

#[derive(Debug)]
struct Hits {
    since: Option<i64>,
    lossy: bool,
    storage: StorageStatus,
    hits: Vec<(String, u64)>,
}

/// Asks for a snapshot and returns the `RuleHits` in its answer.
async fn snapshot(bridge: &RunningBridge, rx: &mut broadcast::Receiver<ServerMessage>) -> Hits {
    while rx.try_recv().is_ok() {}
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::RuleHits {
                since_unix_ms,
                lossy,
                storage,
                hits,
                ..
            } = rx.recv().await.expect("broadcast closed")
            {
                return Hits {
                    since: since_unix_ms,
                    lossy,
                    storage,
                    hits: hits.into_iter().map(|h| (h.name, h.count)).collect(),
                };
            }
        }
    })
    .await
    .expect("no RuleHits in the snapshot answer")
}

fn rule(name: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        action: "allow".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn event(rule_name: &str) -> Event {
    Event {
        rule: Some(Rule {
            name: rule_name.into(),
            ..Default::default()
        }),
        unixnano: 1_700_000_000_000_000_000,
        ..Default::default()
    }
}

/// The daemon's end: subscribed with `rules`, HELLO sent. Keep it alive.
struct Daemon {
    mock: MockOpensnitchd,
    _replies: mpsc::Sender<snitchwatch_proto::protocol::NotificationReply>,
    _inbound: mpsc::Receiver<snitchwatch_proto::protocol::Notification>,
    pings: u64,
    /// `Statistics.rule_hits`: one more for every event sent (`stats.go`).
    rule_hits: u64,
}

impl Daemon {
    async fn connect(bridge: &RunningBridge, rules: &[&str]) -> Self {
        let mut mock = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
            .await
            .unwrap();
        mock.subscribe_with_config(ClientConfig {
            name: "mock".into(),
            rules: rules.iter().map(|n| rule(n)).collect(),
            ..Default::default()
        })
        .await
        .unwrap();
        let (replies, inbound) = mock.open_notifications().await.unwrap();
        let mut ready = bridge.daemon_stream_ready();
        tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
            .await
            .expect("no HELLO")
            .unwrap();
        Self {
            mock,
            _replies: replies,
            _inbound: inbound,
            pings: 0,
            rule_hits: 0,
        }
    }

    async fn ping(&mut self, uptime: u64, events: Vec<Event>) {
        self.pings += 1;
        self.rule_hits += events.len() as u64;
        self.mock
            .ping_with_stats(
                self.pings,
                Statistics {
                    uptime,
                    rule_hits: self.rule_hits,
                    events,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
}

fn pair(name: &str, count: u64) -> (String, u64) {
    (name.to_string(), count)
}

fn persistent() -> StorageStatus {
    StorageStatus {
        persistent: true,
        reason: None,
        unreadable: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_snapshot_answer_carries_rule_hits_before_the_first_ping() {
    let sockets = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let state = state.path().canonicalize().unwrap();
    let bridge = start(Storage::Persistent(state), sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let first = snapshot(&bridge, &mut rx).await;
    assert_eq!(first.since, None, "counting hasn't started");
    assert!(!first.lossy);
    assert!(first.hits.is_empty());
    assert_eq!(first.storage, persistent());
    bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn counts_persist_across_a_restart_and_only_rules_in_the_first_snapshot_return() {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();

    let bridge = start(Storage::Persistent(state.clone()), sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut daemon = Daemon::connect(&bridge, &["a", "b"]).await;
    daemon
        .ping(10, vec![event("a"), event("a"), event("a"), event("b")])
        .await;
    let before = snapshot(&bridge, &mut rx).await;
    assert_eq!(before.hits, vec![pair("a", 3), pair("b", 1)]);
    assert!(!before.lossy, "nothing suggests a missed event yet");
    let started = before.since.expect("counting started at the first ping");

    // The ticker, not just the snapshot answer, delivers changes.
    daemon.ping(11, vec![event("a")]).await;
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::RuleHits { hits, .. } = rx.recv().await.unwrap() {
                if hits.iter().any(|h| h.name == "a" && h.count == 4) {
                    return;
                }
            }
        }
    })
    .await
    .expect("no RuleHits broadcast after a ping");

    bridge.shutdown();
    drop(daemon);
    let file = state.join("rule_hits.json");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );

    // A new bridge process on the same directory.
    let second_sockets = tempfile::tempdir().unwrap();
    let bridge = start(Storage::Persistent(state.clone()), second_sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let restored = snapshot(&bridge, &mut rx).await;
    assert_eq!(restored.since, Some(started), "the persisted start time");
    assert!(restored.lossy, "the bridge was down in between");
    assert!(
        restored.hits.is_empty(),
        "nothing is shown until the daemon's snapshot says which rules exist"
    );
    assert_eq!(restored.storage, persistent());

    let _daemon = Daemon::connect(&bridge, &["a"]).await;
    let adopted = snapshot(&bridge, &mut rx).await;
    assert_eq!(adopted.hits, vec![pair("a", 4)], "b is gone");
    bridge.shutdown();

    // And b stays gone when a later snapshot lists it again.
    let third_sockets = tempfile::tempdir().unwrap();
    let bridge = start(Storage::Persistent(state), third_sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let _daemon = Daemon::connect(&bridge, &["a", "b"]).await;
    assert_eq!(snapshot(&bridge, &mut rx).await.hits, vec![pair("a", 4)]);
    bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_restart_is_a_gap_and_does_not_zero_the_counts() {
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run(config(sockets.path())).await.unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut daemon = Daemon::connect(&bridge, &["a"]).await;
    daemon.ping(500, vec![event("a")]).await;
    daemon.ping(501, vec![event("a")]).await;
    assert!(!snapshot(&bridge, &mut rx).await.lossy);
    daemon.ping(3, vec![event("a")]).await;
    let after = snapshot(&bridge, &mut rx).await;
    assert!(after.lossy);
    assert_eq!(after.hits, vec![pair("a", 3)]);
    bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_usable_state_directory_the_counts_stay_in_memory_and_clients_are_told() {
    for (storage, reason) in [
        (Storage::Ephemeral(EphemeralReason::NotConfigured), None),
        (
            Storage::Ephemeral(EphemeralReason::Unusable("state directory /x: gone".into())),
            Some("state directory /x: gone".to_string()),
        ),
    ] {
        let sockets = tempfile::tempdir().unwrap();
        let bridge = start(storage, sockets.path()).await;
        let mut rx = bridge.broadcast_tx.subscribe();
        let mut daemon = Daemon::connect(&bridge, &["a"]).await;
        daemon.ping(10, vec![event("a")]).await;
        let got = snapshot(&bridge, &mut rx).await;
        assert_eq!(got.hits, vec![pair("a", 1)], "counted in memory");
        assert!(!got.storage.persistent);
        assert_eq!(got.storage.reason, reason);
        bridge.shutdown();
    }

    // An in-process `run` (tests, the Tauri shell) never persists.
    let sockets = tempfile::tempdir().unwrap();
    let bridge = run(config(sockets.path())).await.unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let got = snapshot(&bridge, &mut rx).await;
    assert!(!got.storage.persistent);
    bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_file_is_left_alone_and_the_counts_stay_in_memory() {
    let sockets = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let state = state.path().canonicalize().unwrap();
    let file = state.join("rule_hits.json");
    std::fs::write(&file, b"{ not json").unwrap();

    let bridge = start(Storage::Persistent(state), sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut daemon = Daemon::connect(&bridge, &["a"]).await;
    daemon.ping(10, vec![event("a")]).await;
    let got = snapshot(&bridge, &mut rx).await;
    assert_eq!(got.hits, vec![pair("a", 1)]);
    assert!(!got.storage.persistent);
    assert!(got
        .storage
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("rule hit counts file")));
    bridge.shutdown();
    assert_eq!(std::fs::read(&file).unwrap(), b"{ not json");
}
