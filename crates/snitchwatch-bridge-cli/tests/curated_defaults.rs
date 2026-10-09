//! The recommended background-service rules end to end (prompt-slot D):
//! a system bridge with a persistent state directory, driven over its
//! inbound channel like a GUI, and the mock daemon, which checks every rule
//! the way opensnitchd compiles it.

use std::path::PathBuf;
use std::time::Duration;

use mock_opensnitchd::lists::{spawn_responder, ListsPolicy};
use mock_opensnitchd::loader::{spawn_loader_responder, LoaderModel, SharedLoader};
use mock_opensnitchd::round_trip::as_daemon_reports;
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
const DNS: &str = "dns-resolved";
const DNS_RULE: &str = "snitchwatch-default-dns-resolved";

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

/// A daemon (re)starting from `model`, answering as its loader would.
async fn connect_model(
    bridge: &RunningBridge,
    generation: u64,
    model: &SharedLoader,
) -> (MockOpensnitchd, mpsc::Receiver<Notification>) {
    let mut daemon = MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
        .await
        .unwrap();
    let rules = model.lock().unwrap().snapshot();
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
    let seen = spawn_loader_responder(model.clone(), replies, inbound);
    let mut ready = bridge.daemon_stream_ready();
    tokio::time::timeout(WAIT, ready.wait_for(|g| *g >= generation))
        .await
        .expect("no HELLO")
        .unwrap();
    (daemon, seen)
}

/// Watch the rule list until `done` holds for the names in a `SetRules`
/// and the `leftOnDisk` count of the `RulesNotShown` after it.
async fn rules_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&[String], u32) -> bool,
) {
    tokio::time::timeout(WAIT, async {
        let mut names: Option<Vec<String>> = None;
        loop {
            match rx.recv().await {
                Ok(ServerMessage::SetRules { rules }) => {
                    names = Some(
                        rules
                            .iter()
                            .map(|r| r["name"].as_str().unwrap().to_string())
                            .collect(),
                    );
                }
                Ok(ServerMessage::RulesNotShown { left_on_disk, .. })
                    if names.as_ref().is_some_and(|n| done(n, left_on_disk)) =>
                {
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// Once the bridge has settled, the flatpak entry as a fresh snapshot shows
/// it: not a passing status announced on the way.
async fn settled_entry(
    s: &mut Setup,
    seen: &mut mpsc::Receiver<Notification>,
) -> CuratedDefaultSummary {
    nothing_sent(seen).await;
    while s.rx.try_recv().is_ok() {}
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    entry_until(&mut s.rx, "a snapshot's entry", |_, _| true).await
}

async fn send(bridge: &RunningBridge, msg: ClientMessage) {
    bridge.inbound_tx.send(msg).await.unwrap();
}

fn turn(on: bool) -> ClientMessage {
    turn_entry(FLATPAK, on)
}

fn turn_entry(id: &str, on: bool) -> ClientMessage {
    ClientMessage::SetCuratedDefaults {
        ids: vec![id.into()],
        on,
    }
}

/// Watch `SetCuratedDefaults` until the flatpak entry satisfies `done`.
async fn entry_until(
    rx: &mut broadcast::Receiver<ServerMessage>,
    what: &str,
    done: impl Fn(&CuratedDefaultSummary, &Option<String>) -> bool,
) -> CuratedDefaultSummary {
    entry_until_id(rx, FLATPAK, what, done).await
}

/// Watch `SetCuratedDefaults` until the entry `id` satisfies `done`.
async fn entry_until_id(
    rx: &mut broadcast::Receiver<ServerMessage>,
    id: &str,
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
                let entry = entries.into_iter().find(|e| e.id == id).unwrap();
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

/// Plan D's mock-daemon case: the daemon reports Snitchwatch's copy back
/// (still installed, nothing sent), then an edited copy, which is left
/// alone, even on opt-out.
#[tokio::test]
async fn a_copy_the_user_edited_is_left_alone() {
    let mut s = start(BridgeMode::System).await;
    let (daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn(true)).await;
    let installed = next_command(&mut seen).await.rules[0].clone();
    entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // The daemon restarts and reports our copy, in its own shape (code
    // review M4): nothing to do.
    let (daemon, mut seen) =
        connect_daemon(&s.bridge, 2, vec![as_daemon_reports(&installed)]).await;
    nothing_sent(&mut seen).await;
    while s.rx.try_recv().is_ok() {}
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    entry_until(&mut s.rx, "still installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;

    // The Rules page can still turn the daemon's copy off.
    let mut off = export_rule(&as_daemon_reports(&installed));
    off["enabled"] = false.into();
    send(
        &s.bridge,
        ClientMessage::UpdateRule {
            rule_id: FLATPAK_RULE.into(),
            rule: off,
            request_id: None,
            reply: None,
        },
    )
    .await;
    let toggled = next_command(&mut seen).await;
    assert_eq!(toggled.r#type, Action::ChangeRule as i32);
    assert!(!toggled.rules[0].enabled);
    drop((daemon, seen));

    // The user changed its port outside Snitchwatch.
    let mut edited = as_daemon_reports(&installed);
    edited.operator.as_mut().unwrap().list[2].data = "8443".into();
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 3, vec![edited]).await;
    entry_until(&mut s.rx, "edited by you", |e, _| {
        e.status == EntryStatus::EditedByYou
    })
    .await;
    send(&s.bridge, turn(false)).await;
    entry_until(&mut s.rx, "off, edit kept", |e, _| {
        !e.on && e.status == EntryStatus::EditedByYou
    })
    .await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// Code review H1: choices that can't be read leave the firewall as it is.
#[tokio::test]
async fn unreadable_choices_leave_the_firewall_as_it_is() {
    let sockets = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().canonicalize().unwrap();
    std::fs::write(state.join("curated-defaults.json"), "not json").unwrap();
    let bridge = run_with_options(
        BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_socket_path: sockets.path().join("bridge.sock"),
            cache_capacity: 64,
        },
        RunOptions {
            storage: Storage::Persistent(state.clone()),
            blocklist_fetcher: None,
            mode: BridgeMode::System,
        },
    )
    .await
    .unwrap();
    let mut rx = bridge.broadcast_tx.subscribe();
    let flatpak = snitchwatch_bridge::curated::entries()
        .iter()
        .find(|e| e.id == FLATPAK)
        .unwrap()
        .rule();
    let (_daemon, mut seen) = connect_daemon(&bridge, 1, vec![flatpak]).await;
    send(&bridge, turn(false)).await;
    let entry = entry_until(&mut rx, "inert", |e, unavailable| {
        e.status == EntryStatus::InFirewall
            && unavailable.as_deref()
                == Some(snitchwatch_bridge::curated::manager::UNREADABLE_REASON)
    })
    .await;
    assert_eq!(entry.status, EntryStatus::InFirewall);
    nothing_sent(&mut seen).await;
    assert_eq!(
        std::fs::read_to_string(state.join("curated-defaults.json")).unwrap(),
        "not json"
    );
    bridge.shutdown();
}

/// Tower r12: the daemon refuses to delete a rule whose file is immutable,
/// after dropping it from memory. The entry and the rule list say so, and
/// turning the entry on again installs it again (a `CHANGE_RULE` that
/// rewrites the file), so the next turn-off removes the file for real.
#[tokio::test]
async fn a_refused_delete_is_reported_honestly_and_turning_on_installs_again() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::default().shared();
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    send(&s.bridge, turn(true)).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::ChangeRule as i32
    );
    entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;

    // `chattr +i` on its file, then off: the daemon drops it from memory
    // and fails on the file.
    model.lock().unwrap().stuck.insert(FLATPAK_RULE.into());
    send(&s.bridge, turn(false)).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    entry_until(&mut s.rx, "off, file left", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    let off = settled_entry(&mut s, &mut seen).await;
    assert_eq!(off.status, EntryStatus::OffFileLeft);
    assert!(!off.on);
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    rules_until(&mut s.rx, "unlisted, file noted", |names, left| {
        !names.iter().any(|n| n == FLATPAK_RULE) && left == 1
    })
    .await;
    {
        let model = model.lock().unwrap();
        assert!(!model.memory.contains_key(FLATPAK_RULE));
        assert!(model.files.contains_key(FLATPAK_RULE));
    }
    nothing_sent(&mut seen).await;

    // The attribute cleared, on again: installed again, file rewritten.
    model.lock().unwrap().stuck.clear();
    send(&s.bridge, turn(true)).await;
    let install = next_command(&mut seen).await;
    assert_eq!(install.r#type, Action::ChangeRule as i32);
    assert_eq!(install.rules[0].name, FLATPAK_RULE);
    entry_until(&mut s.rx, "installed again", |e, _| {
        e.status == EntryStatus::Installed && e.on
    })
    .await;
    assert!(model.lock().unwrap().memory.contains_key(FLATPAK_RULE));

    // Off again: this delete removes the file.
    send(&s.bridge, turn(false)).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    entry_until(&mut s.rx, "off", |e, _| e.status == EntryStatus::Off).await;
    {
        let model = model.lock().unwrap();
        assert!(model.memory.is_empty(), "{:?}", model.memory.keys());
        assert!(model.files.is_empty(), "{:?}", model.files.keys());
    }
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// The heal: a daemon restart loads the file again, the rule is listed
/// again, and one delete goes out. While the file is still immutable that
/// delete is refused again, and nothing more is sent (#105: no hot loop).
#[tokio::test]
async fn a_file_left_behind_is_deleted_once_when_the_daemon_loads_it_again() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::default().shared();
    let (daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    send(&s.bridge, turn(true)).await;
    next_command(&mut seen).await;
    entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    model.lock().unwrap().stuck.insert(FLATPAK_RULE.into());
    send(&s.bridge, turn(false)).await;
    next_command(&mut seen).await;
    entry_until(&mut s.rx, "off, file left", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    drop((daemon, seen));

    // Restarted, file still immutable: one delete, refused, then quiet.
    model.lock().unwrap().restart();
    let (daemon, mut seen) = connect_model(&s.bridge, 2, &model).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    entry_until(&mut s.rx, "off, file left again", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    let settled = settled_entry(&mut s, &mut seen).await;
    assert_eq!(settled.status, EntryStatus::OffFileLeft);
    nothing_sent(&mut seen).await;
    drop((daemon, seen));

    // Restarted with the attribute cleared: one delete removes the file.
    model.lock().unwrap().stuck.clear();
    model.lock().unwrap().restart();
    let (_daemon, mut seen) = connect_model(&s.bridge, 3, &model).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    entry_until(&mut s.rx, "off", |e, _| e.status == EntryStatus::Off).await;
    assert!(model.lock().unwrap().files.is_empty());
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// Turn the entry on with the model's daemon, as an install the daemon
/// refused: stock `replaceUserRule` takes the rule into memory before
/// `Save` fails, so the allow applies though the answer is `ERROR`.
async fn refused_install_that_applies(
    s: &mut Setup,
    seen: &mut mpsc::Receiver<Notification>,
    model: &SharedLoader,
) {
    send(&s.bridge, turn(true)).await;
    assert_eq!(next_command(seen).await.r#type, Action::ChangeRule as i32);
    entry_until(&mut s.rx, "not installed", |e, _| {
        e.status == EntryStatus::NotInstalled
    })
    .await;
    assert!(
        model.lock().unwrap().memory.contains_key(FLATPAK_RULE),
        "the refused install applies"
    );
}

/// PR #119 review M1: on again while the file is still immutable, the
/// refused install applies anyway. Off again deletes it, once (the marker
/// from the first refusal doesn't stop it); more offs send nothing; on
/// again, once the file can be written, installs once.
#[tokio::test]
async fn off_after_an_install_refused_on_a_stuck_file_deletes_what_it_applied() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::default().shared();
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    send(&s.bridge, turn(true)).await;
    next_command(&mut seen).await;
    entry_until(&mut s.rx, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    model.lock().unwrap().stuck.insert(FLATPAK_RULE.into());
    send(&s.bridge, turn(false)).await;
    next_command(&mut seen).await;
    entry_until(&mut s.rx, "off, file left", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    refused_install_that_applies(&mut s, &mut seen, &model).await;

    send(&s.bridge, turn(false)).await;
    let delete = next_command(&mut seen).await;
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules[0].name, FLATPAK_RULE);
    entry_until(&mut s.rx, "off, file left", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    assert!(!model.lock().unwrap().memory.contains_key(FLATPAK_RULE));
    for _ in 0..2 {
        send(&s.bridge, turn(false)).await;
        nothing_sent(&mut seen).await;
    }

    model.lock().unwrap().stuck.clear();
    send(&s.bridge, turn(true)).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::ChangeRule as i32
    );
    entry_until(&mut s.rx, "installed again", |e, _| {
        e.status == EntryStatus::Installed && e.on
    })
    .await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// M1 with no marker: the rules directory can't take the file (read-only
/// or full), so the very first install is refused after memory took it.
/// Off deletes it, once.
#[tokio::test]
async fn off_after_an_install_refused_on_save_deletes_what_it_applied() {
    let mut s = start(BridgeMode::System).await;
    let model = LoaderModel::default().shared();
    model.lock().unwrap().stuck.insert(FLATPAK_RULE.into());
    let (_daemon, mut seen) = connect_model(&s.bridge, 1, &model).await;
    refused_install_that_applies(&mut s, &mut seen, &model).await;

    send(&s.bridge, turn(false)).await;
    assert_eq!(
        next_command(&mut seen).await.r#type,
        Action::DeleteRule as i32
    );
    // No file to remove is an `ERROR` too: the harmless over-warning.
    entry_until(&mut s.rx, "off, file left", |e, _| {
        e.status == EntryStatus::OffFileLeft
    })
    .await;
    assert!(model.lock().unwrap().memory.is_empty());
    send(&s.bridge, turn(false)).await;
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// Owner decision S6: the DNS entry is off until chosen, installs exactly the
/// resolver's rule (the mock daemon compiles it like opensnitchd, and the
/// install counts only after its `OK`), and no other entry comes with it.
#[tokio::test]
async fn the_dns_entry_is_off_until_chosen_and_installs_exactly_the_resolver_rule() {
    let mut s = start(BridgeMode::System).await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    nothing_sent(&mut seen).await;

    // Listed with the bridge's own words, off, before any choice.
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    let listed = entry_until_id(&mut s.rx, DNS, "listed", |_, _| true).await;
    assert!(!listed.on);
    assert_eq!(listed.status, EntryStatus::Off);
    assert_eq!(listed.program, "/usr/lib/systemd/systemd-resolved");
    assert_eq!(
        listed.allows,
        "/usr/lib/systemd/systemd-resolved may connect to any address on TCP and UDP port 53, \
         over IPv4 and IPv6."
    );
    assert!(listed.why.contains("does not make lookups private"));
    nothing_sent(&mut seen).await;

    // Turning it on sends one CHANGE_RULE, of exactly that rule.
    send(&s.bridge, turn_entry(DNS, true)).await;
    let change = next_command(&mut seen).await;
    assert_eq!(change.r#type, Action::ChangeRule as i32);
    assert_eq!(change.rules.len(), 1);
    let rule = &change.rules[0];
    assert_eq!(rule.name, DNS_RULE);
    assert_eq!(
        (
            rule.action.as_str(),
            rule.duration.as_str(),
            rule.precedence
        ),
        ("allow", "always", false)
    );
    let leaves: Vec<(&str, &str, &str, bool)> = rule
        .operator
        .as_ref()
        .unwrap()
        .list
        .iter()
        .map(|op| {
            (
                op.r#type.as_str(),
                op.operand.as_str(),
                op.data.as_str(),
                op.sensitive,
            )
        })
        .collect();
    assert_eq!(
        leaves,
        [
            (
                "simple",
                "process.path",
                "/usr/lib/systemd/systemd-resolved",
                true
            ),
            ("simple", "dest.port", "53", false),
            ("regexp", "protocol", "^(tcp|udp)6?$", false),
        ]
    );
    let installed = entry_until_id(&mut s.rx, DNS, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    assert!(installed.on);
    // The other entries stay off, and nothing else is sent.
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    let flatpak = entry_until_id(&mut s.rx, FLATPAK, "flatpak", |_, _| true).await;
    assert_eq!((flatpak.on, flatpak.status), (false, EntryStatus::Off));
    nothing_sent(&mut seen).await;

    // Opt out: one DELETE_RULE of our own rule.
    send(&s.bridge, turn_entry(DNS, false)).await;
    let delete = next_command(&mut seen).await;
    assert_eq!(delete.r#type, Action::DeleteRule as i32);
    assert_eq!(delete.rules[0].name, DNS_RULE);
    nothing_sent(&mut seen).await;
    s.bridge.shutdown();
}

/// The user's side of the DNS entry, as for every entry: a copy deleted
/// outside Snitchwatch stays deleted across a restart, and an edited copy
/// (its port widened to 5353 here) is left alone, even on opt-out.
#[tokio::test]
async fn a_deleted_or_edited_dns_rule_is_left_as_the_user_made_it() {
    let mut s = start(BridgeMode::System).await;
    let (daemon, mut seen) = connect_daemon(&s.bridge, 1, Vec::new()).await;
    send(&s.bridge, turn_entry(DNS, true)).await;
    let installed = next_command(&mut seen).await.rules[0].clone();
    entry_until_id(&mut s.rx, DNS, "installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // The daemon reports our copy back in its own shape: nothing to do.
    let (daemon, mut seen) =
        connect_daemon(&s.bridge, 2, vec![as_daemon_reports(&installed)]).await;
    nothing_sent(&mut seen).await;
    while s.rx.try_recv().is_ok() {}
    send(&s.bridge, ClientMessage::RequestSnapshot).await;
    entry_until_id(&mut s.rx, DNS, "still installed", |e, _| {
        e.status == EntryStatus::Installed
    })
    .await;
    drop((daemon, seen));

    // Edited outside Snitchwatch: left alone, never overwritten or deleted.
    let mut edited = as_daemon_reports(&installed);
    edited.operator.as_mut().unwrap().list[1].data = "5353".into();
    let (daemon, mut seen) = connect_daemon(&s.bridge, 3, vec![edited]).await;
    entry_until_id(&mut s.rx, DNS, "edited by you", |e, _| {
        e.status == EntryStatus::EditedByYou
    })
    .await;
    send(&s.bridge, turn_entry(DNS, false)).await;
    entry_until_id(&mut s.rx, DNS, "off, edit kept", |e, _| {
        !e.on && e.status == EntryStatus::EditedByYou
    })
    .await;
    nothing_sent(&mut seen).await;
    drop((daemon, seen));

    // Deleted outside (turn it on again, then the daemon comes back
    // without it): never reinstalled.
    send(&s.bridge, turn_entry(DNS, true)).await;
    entry_until_id(&mut s.rx, DNS, "on, edit kept", |e, _| {
        e.on && e.status == EntryStatus::EditedByYou
    })
    .await;
    let (_daemon, mut seen) = connect_daemon(&s.bridge, 4, Vec::new()).await;
    entry_until_id(&mut s.rx, DNS, "deleted outside", |e, _| {
        e.status == EntryStatus::DeletedOutside
    })
    .await;
    nothing_sent(&mut seen).await;
    let saved = std::fs::read_to_string(s.state.join("curated-defaults.json")).unwrap();
    assert!(
        saved.contains(r#""deletedByUser":["dns-resolved"]"#),
        "{saved}"
    );
    s.bridge.shutdown();
}
