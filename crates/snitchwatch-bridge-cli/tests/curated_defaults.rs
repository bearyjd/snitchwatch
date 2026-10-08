//! The recommended background-service rules end to end (prompt-slot D):
//! a system bridge with a persistent state directory, driven over its
//! inbound channel like a GUI, and the mock daemon, which checks every rule
//! the way opensnitchd compiles it.

use std::path::PathBuf;
use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::curated::reconcile::EntryStatus;
use snitchwatch_bridge::curated::wire::CuratedDefaultSummary;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::curated_storage::PER_USER_REASON;
use snitchwatch_bridge_cli::{
    run_with_options, BridgeConfig, BridgeMode, RunOptions, RunningBridge, Storage,
};
use snitchwatch_proto::protocol::{Action, ClientConfig, Notification};
use tokio::sync::{broadcast, mpsc};

const WAIT: Duration = Duration::from_secs(10);
const FLATPAK: &str = "flatpak-flathub";
const FLATPAK_RULE: &str = "snitchwatch-default-flatpak-flathub";

struct Setup {
    _sockets: tempfile::TempDir,
    _state_dir: tempfile::TempDir,
    state: PathBuf,
    bridge: RunningBridge,
    rx: broadcast::Receiver<ServerMessage>,
}

async fn start(mode: BridgeMode) -> Setup {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
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

/// A daemon (re)starting with `rules` as its snapshot; returns once its
/// HELLO is the bridge's `generation`th.
async fn connect_daemon(
    bridge: &RunningBridge,
    generation: u64,
    rules: Vec<snitchwatch_proto::protocol::Rule>,
) -> (MockOpensnitchd, mpsc::Receiver<Notification>) {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    daemon
        .subscribe_with_config(ClientConfig {
            id: 1,
            name: "mock".into(),
            version: "mock-1.8.0".into(),
            rules,
            ..Default::default()
        })
        .await
        .unwrap();
    let (replies, inbound) = daemon.open_notifications().await.unwrap();
    let seen = spawn_responder(ListsPolicy::Accept, replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= generation))
        .await
        .expect("no HELLO")
        .unwrap();
    (daemon, seen)
}

async fn send(bridge: &RunningBridge, msg: ClientMessage) {
    bridge.inbound_tx.send(msg).await.unwrap();
}

fn turn(on: bool) -> ClientMessage {
    ClientMessage::SetCuratedDefaults {
        ids: vec![FLATPAK.into()],
        on,
    }
}

/// Watch `SetCuratedDefaults` until the flatpak entry satisfies `done`.
async fn entry_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&CuratedDefaultSummary, &Option<String>) -> bool,
) -> CuratedDefaultSummary {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(ServerMessage::SetCuratedDefaults {
                entries,
                unavailable,
                ..
            }) = rx.recv().await
            {
                let entry = entries.into_iter().find(|e| e.id == FLATPAK).unwrap();
                if done(&entry, &unavailable) {
                    return entry;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}

async fn next_command(seen: &mut mpsc::Receiver<Notification>) -> Notification {
    tokio::time::timeout(WAIT, seen.recv())
        .await
        .expect("no command reached the daemon")
        .unwrap()
}

async fn nothing_sent(seen: &mut mpsc::Receiver<Notification>) {
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(seen.try_recv().is_err(), "a command reached the daemon");
}

#[tokio::test]
async fn an_entry_turned_on_is_installed_toggled_and_removed_only_by_the_user() {
    let mut s = start(BridgeMode::System).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    nothing_sent(&mut seen).await;

    // Opt in: one CHANGE_RULE, "installed" only after the daemon's OK.
    send(&s.bridge, turn(true)).await;
    let change = next_command(&mut seen).await;
    assert_eq!(change.r#type, Action::ChangeRule as i32);
    assert_eq!(change.rules[0].name, FLATPAK_RULE);
    let installed = entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    assert!(installed.on);

    // Turned off from the Rules page: the data file's rule, enabled false.
    let mut toggled = export_rule(&change.rules[0]);
    toggled["enabled"] = false.into();
    send(
        &s.bridge,
        ClientMessage::UpdateRule {
            rule_id: FLATPAK_RULE.into(),
            rule: toggled,
            request_id: None,
            reply: None,
        },
    )
    .await;
    let off = next_command(&mut seen).await;
    assert_eq!(off.r#type, Action::ChangeRule as i32);
    assert!(!off.rules[0].enabled);
    entry_until(&mut s.rx, "installed but off", |e, _| {
        e.status == EntryStatus::InstalledButOff
    })
    .await;

    // Opt out: one DELETE_RULE of our own rule.
    send(&s.bridge, turn(false)).await;
    let delete = next_command(&mut seen).await;
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules[0].name, FLATPAK_RULE);
    entry_until(&mut s.rx, "off", |e, _| {
        e.status == EntryStatus::Off && !e.on
    })
    .await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// Plan item 13: a curated rule the user deleted outside Snitchwatch is
/// never reinstalled, across daemon restarts too, and the choice is saved.
#[tokio::test]
async fn a_rule_deleted_outside_snitchwatch_is_not_reinstalled_after_a_restart() {
    let mut s = start(BridgeMode::System).await;
    let (daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn(true)).await;
    next_command(&mut seen).await;
    entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // The daemon comes back without the rule: the user deleted it.
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 2, Vec::new()).await;
    entry_until(&mut s.rx, "deleted outside", |e, _| {
        e.status == EntryStatus::DeletedOutside
    })
    .await;
    nothing_sent(&mut seen).await;
    let saved = std::fs::read_to_string(s.state.join("curated-defaults.json")).unwrap();
    assert!(
        saved.contains(r#""deletedByUser":["flatpak-flathub"]"#),
        "{saved}"
    );
    s.bridge.shutdown();
}

#[tokio::test]
async fn the_per_user_bridge_lists_them_but_never_installs_any() {
    let mut s = start(BridgeMode::User).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn(true)).await;
    let entry = entry_until(&mut s.rx, "unavailable", |_, unavailable| {
        unavailable.as_deref() == Some(PER_USER_REASON)
    })
    .await;
    assert!(!entry.on);
    assert_eq!(entry.status, EntryStatus::Unavailable);
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}
