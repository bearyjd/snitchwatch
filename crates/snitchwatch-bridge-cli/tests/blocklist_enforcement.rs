//! Blocklist enforcement end to end (issue #45 PR B): a bridge with a
//! persistent state directory, a test fetcher (no network) and the mock
//! daemon, which accepts or refuses `lists` rules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mock_opensnitchd::lists::{
    spawn_responder, validate_blocklist_rule, ListsPolicy, LISTS_REFUSAL,
};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use snitchwatch_bridge::rule_policy::BLOCKLIST_MANAGED_REASON;
use snitchwatch_bridge::ws_messages::{
    BlocklistSummary, ClientMessage, ServerMessage, ENFORCEMENT_NOT_ENFORCED,
    ENFORCEMENT_RULE_INSTALLED,
};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, EphemeralReason, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{Action, Notification};
use tokio::sync::{broadcast, mpsc};

const LIST_URL: &str = "https://lists.invalid/ads.txt";
const LIST_BODY: &str = "0.0.0.0 ads.example\n0.0.0.0 tracker.example\n0.0.0.0 203.0.113.7\n";
const WAIT: Duration = Duration::from_secs(10);

struct Fetcher;

#[async_trait::async_trait]
impl BlocklistFetch for Fetcher {
    async fn fetch(&self, _url: &str) -> FetchOutcome {
        process_body(LIST_BODY)
    }
}

struct Setup {
    _sockets: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    state: PathBuf,
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
}

async fn start(persistent: bool) -> Setup {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    let storage = if persistent {
        Storage::Persistent(state.clone())
    } else {
        Storage::Ephemeral(EphemeralReason::InProcess)
    };
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage,
            blocklist_fetcher: Some(Arc::new(Fetcher)),
        },
    )
    .await
    .unwrap();
    let rx = bridge.broadcast_tx.subscribe();
    Setup {
        _sockets: sockets,
        _state_dir: state_dir,
        state,
        bridge,
        rx,
    }
}

/// Connect the mock daemon (empty rule snapshot) and wait for its HELLO.
async fn connect_daemon(
    bridge: &RunningBridge,
    policy: ListsPolicy,
) -> (MockOpensnitchd, mpsc::Receiver<Notification>) {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon.subscribe("mock").await.unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_responder(policy, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    (daemon, seen)
}

async fn subscribe(bridge: &RunningBridge) {
    bridge
        .inbound_tx
        .send(ClientMessage::SubscribeBlocklist {
            url: LIST_URL.into(),
        })
        .await
        .unwrap();
}

async fn next_command(seen: &mut mpsc::Receiver<Notification>) -> Notification {
    tokio::time::timeout(WAIT, seen.recv())
        .await
        .expect("no command reached the daemon")
        .unwrap()
}

/// Watch `SetBlocklists` until the only list satisfies `done`.
async fn list_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&BlocklistSummary) -> bool,
) -> BlocklistSummary {
    tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetBlocklists { blocklists, .. } = rx.recv().await.unwrap() {
                if let Some(list) = blocklists.into_iter().find(|l| done(l)) {
                    return list;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}

fn lists_root(state: &Path) -> PathBuf {
    state.join("blocklists")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_installs_its_lists_rules_and_reports_them_installed() {
    let mut setup = start(true).await;
    let (state, bridge, rx) = (setup.state.clone(), &setup.bridge, &mut setup.rx);
    let (_daemon, mut seen) = connect_daemon(bridge, ListsPolicy::Accept).await;
    subscribe(bridge).await;

    let domains = next_command(&mut seen).await;
    assert_eq!(domains.r#type, Action::ChangeRule as i32);
    let loaded = validate_blocklist_rule(&domains.rules[0], &lists_root(&state)).unwrap();
    assert_eq!(
        loaded.into_iter().collect::<Vec<_>>(),
        vec!["ads.example", "tracker.example"]
    );
    let ips = next_command(&mut seen).await;
    let loaded = validate_blocklist_rule(&ips.rules[0], &lists_root(&state)).unwrap();
    assert_eq!(loaded.into_iter().collect::<Vec<_>>(), vec!["203.0.113.7"]);

    let list = list_until(rx, "rule installed", |l| {
        l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    assert_eq!(list.enforcement_reason, None);

    // The Rules page lists both rules read-only, managed on the Blocklists
    // page, and a GUI can't delete one.
    let name = domains.rules[0].name.clone();
    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .unwrap();
    let rules = tokio::time::timeout(WAIT, async {
        loop {
            if let ServerMessage::SetRules { rules } = rx.recv().await.unwrap() {
                if rules.iter().any(|r| r["name"] == name.as_str()) {
                    return rules;
                }
            }
        }
    })
    .await
    .expect("no SetRules with the blocklist rule");
    let row = rules.iter().find(|r| r["name"] == name.as_str()).unwrap();
    assert_eq!(row["readOnlyReason"], BLOCKLIST_MANAGED_REASON);
    assert_eq!(row["deletable"], false);
    bridge
        .inbound_tx
        .send(ClientMessage::DeleteRule { rule_id: name })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), seen.recv())
            .await
            .is_err(),
        "a GUI's delete of a blocklist rule reached the daemon"
    );

    // Unsubscribe: both rules are deleted, then the list's directory.
    let list_dir = lists_root(&state).join(&list.id);
    assert!(list_dir.is_dir());
    bridge
        .inbound_tx
        .send(ClientMessage::UnsubscribeBlocklist {
            id: list.id.clone(),
        })
        .await
        .unwrap();
    for kind in ["domains", "ips"] {
        let delete = next_command(&mut seen).await;
        assert_eq!(delete.r#type, Action::DeleteRule as i32);
        assert_eq!(
            delete.rules[0].name,
            format!("z00-blocklist:{}:{kind}", list.id)
        );
    }
    let deadline = tokio::time::Instant::now() + WAIT;
    while list_dir.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the directory stayed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    setup.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_refuses_lists_rules_leaves_the_list_not_enforced_saying_why() {
    let mut setup = start(true).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    let (_daemon, _seen) =
        connect_daemon(bridge, ListsPolicy::RefuseLists(LISTS_REFUSAL.into())).await;
    subscribe(bridge).await;
    let list = list_until(rx, "a refusal", |l| {
        l.enforcement == ENFORCEMENT_NOT_ENFORCED && l.enforcement_reason.is_some()
    })
    .await;
    let reason = list.enforcement_reason.unwrap();
    assert!(
        reason.starts_with("The firewall service refused"),
        "{reason}"
    );
    assert!(reason.contains(LISTS_REFUSAL), "{reason}");
    setup.bridge.shutdown();
}

/// Subscribed while no daemon is connected: nothing is sent, the list says
/// so, and the reconcile after the daemon's first snapshot installs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_connects_later_gets_the_rule_from_the_reconcile() {
    let mut setup = start(true).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    subscribe(bridge).await;
    let list = list_until(rx, "not connected", |l| {
        l.enforcement == ENFORCEMENT_NOT_ENFORCED
    })
    .await;
    assert!(
        list.enforcement_reason
            .as_deref()
            .is_some_and(|r| r.contains("isn't connected")),
        "{list:?}"
    );
    let (_daemon, mut seen) = connect_daemon(bridge, ListsPolicy::Accept).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::ChangeRule as i32
    );
    list_until(rx, "installed by the reconcile", |l| {
        l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    setup.bridge.shutdown();
}

/// B0: an in-process bridge has no state directory: no list file, no rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_state_directory_no_file_or_rule_is_installed() {
    let mut setup = start(false).await;
    let (state, bridge, rx) = (setup.state.clone(), &setup.bridge, &mut setup.rx);
    let (_daemon, mut seen) = connect_daemon(bridge, ListsPolicy::Accept).await;
    subscribe(bridge).await;
    let list = list_until(rx, "downloaded", |l| l.status == "ok").await;
    assert_eq!(list.enforcement, ENFORCEMENT_NOT_ENFORCED);
    assert_eq!(
        list.enforcement_reason.as_deref(),
        Some("no state directory: in-process")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(500), seen.recv())
            .await
            .is_err(),
        "a rule was sent without a state directory"
    );
    assert_eq!(std::fs::read_dir(&state).unwrap().count(), 0);
    setup.bridge.shutdown();
}
