//! Blocklist enforcement end to end (issue #45 PR B): a bridge with a
//! persistent state directory, a test fetcher (no network) and the mock
//! daemon, which accepts or refuses `lists` rules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mock_opensnitchd::lists::{
    spawn_observing_responder, spawn_responder, validate_blocklist_rule, ListsPolicy, LISTS_REFUSAL,
};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use snitchwatch_bridge::rule_policy::BLOCKLIST_MANAGED_REASON;
use snitchwatch_bridge::ws_messages::{
    BlocklistSummary, ClientMessage, ServerMessage, ENFORCEMENT_NOT_ENFORCED, ENFORCEMENT_PENDING,
    ENFORCEMENT_RULE_INSTALLED,
};
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, EphemeralReason, RunOptions, RunningBridge,
    Storage, PER_USER_REASON,
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
    start_as(persistent, BridgeMode::System).await
}

async fn start_as(persistent: bool, mode: BridgeMode) -> Setup {
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
            mode,
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
        .send(ClientMessage::DeleteRule {
            rule_id: name,
            request_id: None,
            reply: None,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), seen.recv())
            .await
            .is_err(),
        "a GUI's delete of a blocklist rule reached the daemon"
    );

    setup.bridge.shutdown();
}

/// Unsubscribe deletes both rules while the list's directory still exists, and
/// keeps the directory for a while (opensnitchd reloads a path at most every
/// 30 s and clears a list whose file is missing, issue #73): subscribing
/// again at once finds the files in place, and rewrites none of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribing_deletes_the_rules_and_a_quick_resubscribe_finds_the_files_in_place() {
    use std::os::unix::fs::MetadataExt;

    let mut setup = start(true).await;
    let (state, bridge, rx) = (setup.state.clone(), &setup.bridge, &mut setup.rx);
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon.subscribe("mock").await.unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let mut seen =
        spawn_observing_responder(ListsPolicy::Accept, lists_root(&state), replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= 1))
        .await
        .expect("no HELLO")
        .unwrap();
    subscribe(bridge).await;
    let list = list_until(rx, "rule installed", |l| {
        l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    let list_dir = lists_root(&state).join(&list.id);
    let domains_file = list_dir.join("domains").join("domains.list");
    assert!(domains_file.is_file());
    let inode = std::fs::metadata(&domains_file).unwrap().ino();
    while seen.try_recv().is_ok() {}

    bridge
        .inbound_tx
        .send(ClientMessage::UnsubscribeBlocklist {
            id: list.id.clone(),
        })
        .await
        .unwrap();
    let mut deleted = Vec::new();
    while deleted.len() < 2 {
        let (command, dir_existed) = tokio::time::timeout(WAIT, seen.recv())
            .await
            .expect("no DELETE_RULE")
            .unwrap();
        if command.r#type == Action::DeleteRule as i32 {
            assert!(dir_existed, "the list directory went before its rule");
            deleted.push(command.rules[0].name.clone());
        }
    }
    assert_eq!(
        deleted,
        vec![
            format!("z00-blocklist:{}:domains", list.id),
            format!("z00-blocklist:{}:ips", list.id),
        ]
    );
    // The reconcile after the unsubscribe has run its orphan purge by now; the
    // files are still there.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(domains_file.is_file(), "the files went with the rules");

    // Subscribing again puts the rules back over the very same files.
    subscribe(bridge).await;
    list_until(rx, "rule installed again", |l| {
        l.id == list.id && l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    assert_eq!(
        std::fs::metadata(&domains_file).unwrap().ino(),
        inode,
        "the list file was rewritten"
    );
    setup.bridge.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_refuses_lists_rules_leaves_the_list_not_enforced_saying_why() {
    let mut setup = start(true).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    let (_daemon, mut seen) =
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
    // One subscribe, one try: the clean-up reconcile after it doesn't retry
    // (a refresh tick or a new daemon connection does).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut changes = 0;
    while let Ok(command) = seen.try_recv() {
        changes += usize::from(command.r#type == Action::ChangeRule as i32);
    }
    assert_eq!(changes, 1, "a refused list was retried at once");
    setup.bridge.shutdown();
}

/// Subscribed while no daemon is connected: nothing is sent, the list says
/// so, and the reconcile after the daemon's first snapshot installs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_connects_later_gets_the_rule_from_the_reconcile() {
    let mut setup = start(true).await;
    let (bridge, rx) = (&setup.bridge, &mut setup.rx);
    subscribe(bridge).await;
    // Not known either way: "Not confirmed yet", with the reason.
    let list = list_until(rx, "not connected", |l| {
        l.enforcement == ENFORCEMENT_PENDING && l.enforcement_reason.is_some()
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

/// Review M3: a per-user bridge saves subscriptions but installs nothing:
/// root opensnitchd must not read files the user's other apps can replace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_per_user_bridge_installs_no_file_or_rule() {
    let mut setup = start_as(true, BridgeMode::User).await;
    let (state, bridge, rx) = (setup.state.clone(), &setup.bridge, &mut setup.rx);
    let (_daemon, mut seen) = connect_daemon(bridge, ListsPolicy::Accept).await;
    subscribe(bridge).await;
    let list = list_until(rx, "downloaded", |l| l.status == "ok").await;
    assert_eq!(list.enforcement, ENFORCEMENT_NOT_ENFORCED);
    assert_eq!(list.enforcement_reason.as_deref(), Some(PER_USER_REASON));
    assert!(
        tokio::time::timeout(Duration::from_millis(500), seen.recv())
            .await
            .is_err(),
        "a per-user bridge sent a rule"
    );
    assert!(!lists_root(&state).exists());
    assert!(state.join("blocklists.sqlite3").is_file(), "still saved");
    setup.bridge.shutdown();
}

/// Start a system bridge on an existing state directory.
async fn start_on(state: &Path, sockets: &Path) -> RunningBridge {
    run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.to_path_buf()),
            blocklist_fetcher: Some(Arc::new(Fetcher)),
            mode: BridgeMode::System,
        },
    )
    .await
    .unwrap()
}

/// A restarted bridge whose daemon's rule snapshot already holds the list's
/// rules unchanged reports them installed and sends no `CHANGE_RULE` (and
/// rewrites no list file).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_against_an_identical_daemon_snapshot_sends_no_rule() {
    use std::os::unix::fs::MetadataExt;
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    let first_sockets = tempfile::tempdir().unwrap();
    let bridge = start_on(&state, first_sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let (daemon, mut seen) = connect_daemon(&bridge, ListsPolicy::Accept).await;
    subscribe(&bridge).await;
    let installed: Vec<_> = vec![
        next_command(&mut seen).await.rules[0].clone(),
        next_command(&mut seen).await.rules[0].clone(),
    ];
    let list = list_until(&mut rx, "rule installed", |l| {
        l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    let file = lists_root(&state)
        .join(&list.id)
        .join("domains/domains.list");
    let inode = std::fs::metadata(&file).unwrap().ino();
    bridge.shutdown();
    drop(daemon);

    let second_sockets = tempfile::tempdir().unwrap();
    let bridge = start_on(&state, second_sockets.path()).await;
    let mut rx = bridge.broadcast_tx.subscribe();
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon
        .subscribe_with_config(snitchwatch_proto::protocol::ClientConfig {
            name: "mock".into(),
            rules: installed,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let mut seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    list_until(&mut rx, "installed after the restart", |l| {
        l.enforcement == ENFORCEMENT_RULE_INSTALLED
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        seen.try_recv().is_err(),
        "a rule was resent after the restart"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().ino(),
        inode,
        "the list was rewritten"
    );
    bridge.shutdown();
}
