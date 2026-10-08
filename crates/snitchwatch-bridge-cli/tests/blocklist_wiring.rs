//! The bridge's blocklist wiring (issue #45 PR A), driven through
//! `run_with_options` with a test fetcher (no network):
//! - subscribe messages reach the manager, and persist in the state
//!   directory across a restart, where the refresh loop picks them up;
//! - a slow fetch never stalls a verdict;
//! - `SetBlocklists.storage` reports the resolved storage;
//! - only `main.rs` and `run_system` resolve the state directory.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use snitchwatch_bridge::ws_messages::{
    BlocklistSummary, ClientMessage, ServerMessage, StorageStatus, VerdictAction, VerdictDuration,
    VerdictScope, ENFORCEMENT_NOT_ENFORCED,
};
use snitchwatch_bridge_cli::{
    run, run_with_options, BridgeConfig, EphemeralReason, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::Connection;
use tokio::sync::{broadcast, Notify};
use tokio_tungstenite::tungstenite::Message;

const LIST_URL: &str = "https://lists.invalid/ads.txt";
const LIST_BODY: &str = "0.0.0.0 ads.example\n0.0.0.0 tracker.example\n";
const WAIT: Duration = Duration::from_secs(10);

/// Serves [`LIST_BODY`] for every URL, or fails while `offline` is set.
/// Counts calls per URL.
#[derive(Default)]
struct TestFetcher {
    offline: AtomicBool,
    calls: Mutex<HashMap<String, usize>>,
}

impl TestFetcher {
    fn calls(&self, url: &str) -> usize {
        self.calls.lock().unwrap().get(url).copied().unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl BlocklistFetch for TestFetcher {
    async fn fetch(&self, url: &str) -> FetchOutcome {
        *self
            .calls
            .lock()
            .unwrap()
            .entry(url.to_string())
            .or_default() += 1;
        if self.offline.load(Ordering::SeqCst) {
            return FetchOutcome::Failed {
                reason: "offline".into(),
            };
        }
        process_body(LIST_BODY)
    }
}

/// Blocks every fetch until `release` is notified.
#[derive(Default)]
struct GatedFetcher {
    started: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl BlocklistFetch for GatedFetcher {
    async fn fetch(&self, _url: &str) -> FetchOutcome {
        self.started.notify_one();
        self.release.notified().await;
        process_body(LIST_BODY)
    }
}

fn config(dir: &Path) -> BridgeConfig {
    BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.join("bridge.sock"),
        cache_capacity: 64,
    }
}

fn options(storage: Storage, fetcher: Arc<dyn BlocklistFetch>) -> RunOptions {
    RunOptions {
        storage,
        blocklist_fetcher: Some(fetcher),
    }
}

/// Ask for a snapshot and return its `SetBlocklists`.
async fn snapshot(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> (Vec<BlocklistSummary>, Option<StorageStatus>) {
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetBlocklists {
                blocklists,
                storage,
            } = rx.recv().await.expect("broadcast closed")
            {
                return (blocklists, storage);
            }
        }
    })
    .await
    .expect("no SetBlocklists in the snapshot")
}

/// Snapshot until the subscription list satisfies `done`.
async fn snapshot_until(
    bridge: &RunningBridge,
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&[BlocklistSummary]) -> bool,
) -> (Vec<BlocklistSummary>, Option<StorageStatus>) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (lists, storage) = snapshot(bridge, rx).await;
        if done(&lists) {
            return (lists, storage);
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Subscribe while the list is unreachable (so it stays due), restart on the
/// same state directory: the subscription is still there and the startup
/// refresh fetches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_persist_across_a_restart_and_get_refreshed() {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();

    let first_fetcher = Arc::new(TestFetcher::default());
    first_fetcher.offline.store(true, Ordering::SeqCst);
    let bridge = run_with_options(
        config(sockets.path()),
        options(Storage::Persistent(state.clone()), first_fetcher.clone()),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    bridge
        .inbound_tx
        .send(ClientMessage::SubscribeBlocklist {
            url: LIST_URL.into(),
        })
        .await
        .unwrap();
    let (lists, storage) = snapshot_until(&bridge, &mut rx, "the failed first fetch", |l| {
        l.first().is_some_and(|l| l.status == "failed")
    })
    .await;
    assert_eq!(first_fetcher.calls(LIST_URL), 1);
    assert_eq!(
        storage,
        Some(StorageStatus {
            persistent: true,
            reason: None
        })
    );
    assert_eq!(lists.len(), 1);
    bridge.shutdown();
    assert!(state.join("blocklists.sqlite3").is_file());

    let second_fetcher = Arc::new(TestFetcher::default());
    let bridge = run_with_options(
        config(sockets.path()),
        options(Storage::Persistent(state), second_fetcher.clone()),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (refreshed, _) = snapshot_until(&bridge, &mut rx, "the startup refresh", |l| {
        l.first().is_some_and(|l| l.status == "ok")
    })
    .await;
    bridge.shutdown();
    assert_eq!(second_fetcher.calls(LIST_URL), 1);
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].url, LIST_URL);
    assert_eq!(refreshed[0].entry_count, 2);
    // Downloaded, but PR A installs no daemon rule.
    assert_eq!(refreshed[0].enforcement, ENFORCEMENT_NOT_ENFORCED);
    assert_eq!(
        refreshed[0].enforcement_reason.as_deref(),
        Some("no rule sink yet")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_blocklists_reports_the_resolved_storage() {
    let sockets = tempfile::tempdir().unwrap();

    let bridge = run(config(sockets.path())).await.unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (_, storage) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    assert_eq!(
        storage,
        Some(StorageStatus {
            persistent: false,
            reason: None
        }),
        "run() is in-process and never persistent"
    );

    // A directory where the database file should be: the store can't open,
    // the bridge still starts, in memory, and says why.
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    std::fs::create_dir(state.join("blocklists.sqlite3")).unwrap();
    let bridge = run_with_options(
        config(sockets.path()),
        options(Storage::Persistent(state), Arc::new(TestFetcher::default())),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (_, storage) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    let storage = storage.expect("storage status");
    assert!(!storage.persistent);
    assert!(
        storage
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("blocklist store: ")),
        "{storage:?}"
    );

    let bridge = run_with_options(
        config(sockets.path()),
        options(
            Storage::Ephemeral(EphemeralReason::Unusable(
                "unexpected state directory /x".into(),
            )),
            Arc::new(TestFetcher::default()),
        ),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let (_, storage) = snapshot(&bridge, &mut rx).await;
    bridge.shutdown();
    assert_eq!(
        storage,
        Some(StorageStatus {
            persistent: false,
            reason: Some("unexpected state directory /x".into())
        })
    );
}

/// The pump only enqueues blocklist work: with a fetch stuck in flight, a
/// verdict for a pending prompt still applies at once (issue #45).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_fetch_never_stalls_a_verdict() {
    let sockets = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(GatedFetcher::default());
    let bridge = run_with_options(
        config(sockets.path()),
        options(
            Storage::Ephemeral(EphemeralReason::InProcess),
            fetcher.clone(),
        ),
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();

    // `ask_rule` only prompts while an authenticated GUI is attached.
    let transport = tokio::net::UnixStream::connect(&bridge.ws_socket_path)
        .await
        .unwrap();
    let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", transport)
        .await
        .unwrap();
    gui.send(Message::Text(bridge.ws_token.as_str().into()))
        .await
        .unwrap();
    let ack = gui.next().await.unwrap().unwrap();
    assert!(matches!(ack, Message::Text(ref text) if text.contains("authenticated")));

    bridge
        .inbound_tx
        .send(ClientMessage::SubscribeBlocklist {
            url: LIST_URL.into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(WAIT, fetcher.started.notified())
        .await
        .expect("the subscribe never started its fetch");

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask = tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        daemon
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
            .unwrap()
    });
    let pending_id = tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() {
                if let Some(row) = rows.into_iter().find(|row| row.action.is_none()) {
                    break row.id;
                }
            }
        }
    })
    .await
    .expect("no pending row");

    bridge
        .inbound_tx
        .send(ClientMessage::SetVerdict {
            row_id: pending_id.clone(),
            verdict: VerdictAction::Allow,
            scope: VerdictScope::ThisHost,
            duration: Some(VerdictDuration::Once),
            remember: None,
        })
        .await
        .unwrap();
    let applied = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerMessage::UpdateConnectionRows { rows } = rx.recv().await.unwrap() {
                if rows.iter().any(|row| row.id == pending_id) {
                    break;
                }
            }
        }
    })
    .await;
    fetcher.release.notify_one();
    assert!(
        applied.is_ok(),
        "the verdict waited for the blocklist fetch"
    );
    let rule = tokio::time::timeout(WAIT, ask)
        .await
        .expect("AskRule never answered")
        .unwrap();
    assert_eq!(rule.action, "allow");
    bridge.shutdown();
}

/// Lines of `source` with whole-line `//` comments dropped.
fn code(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The state directory is resolved once, by `main.rs` (per-user) and
/// `run_system`. `run()` / `run_with_incoming` never read it, so the Tauri
/// shell and tests can't pick up a stray `STATE_DIRECTORY`.
#[test]
fn only_main_and_run_system_resolve_the_state_directory() {
    let lib = code(include_str!("../src/lib.rs"));
    let main = code(include_str!("../src/main.rs"));
    let calls: Vec<usize> = lib
        .match_indices("resolve_storage(")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(calls.len(), 1, "lib.rs must resolve storage exactly once");
    let start = lib
        .find("pub async fn run_system(")
        .expect("run_system moved");
    let end = start + lib[start..].find("\n}\n").expect("end of run_system");
    assert!(
        (start..end).contains(&calls[0]),
        "lib.rs resolves storage outside run_system"
    );
    assert_eq!(main.matches("resolve_storage(").count(), 1);
    for (name, source) in [
        ("lib.rs", &lib),
        ("main.rs", &main),
        ("activation.rs", &code(include_str!("../src/activation.rs"))),
        ("cli.rs", &code(include_str!("../src/cli.rs"))),
    ] {
        for forbidden in ["var_os(\"STATE_DIRECTORY\")", "var(\"STATE_DIRECTORY\")"] {
            assert!(!source.contains(forbidden), "{name} reads STATE_DIRECTORY");
        }
        for forbidden in [
            "var_os(\"SNITCHWATCH_STATE_DIR\")",
            "var(\"SNITCHWATCH_STATE_DIR\")",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} reads SNITCHWATCH_STATE_DIR"
            );
        }
    }
    assert!(
        !include_str!("../src/activation.rs").contains("resolve_storage"),
        "activation.rs resolves storage"
    );
    let storage = code(include_str!("../src/storage.rs"));
    assert_eq!(
        storage.matches("std::env::var_os(").count(),
        2,
        "storage.rs reads the environment only in resolve_storage"
    );
}
