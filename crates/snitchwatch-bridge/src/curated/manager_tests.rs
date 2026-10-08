//! The curated defaults' worker against a real `DaemonCommands` and rules
//! cache, with a scripted daemon stream answering each command.

use super::*;
use crate::cache::rules::RulesSync;
use crate::curated::check_curated_rule;
use crate::daemon_commands::{DaemonTransport, StreamRegistration};
use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Rule,
};

const FLATPAK: &str = "flatpak-flathub";
const FLATPAK_RULE: &str = "snitchwatch-default-flatpak-flathub";

#[derive(Clone, Copy)]
enum Daemon {
    Accept,
    Refuse,
}

struct Harness {
    _state: tempfile::TempDir,
    file: PathBuf,
    commands: DaemonCommands,
    rules: RulesSync,
    seen: Arc<Mutex<Vec<Notification>>>,
    stream: Option<(StreamRegistration, u64)>,
}

fn reply(id: u64, ok: bool) -> NotificationReply {
    NotificationReply {
        id,
        code: if ok {
            NotificationReplyCode::Ok as i32
        } else {
            NotificationReplyCode::Error as i32
        },
        data: String::new(),
    }
}

impl Harness {
    fn new() -> Self {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
        std::fs::create_dir_all(&base).unwrap();
        let state = tempfile::tempdir_in(base).unwrap();
        let file = state.path().canonicalize().unwrap().join(store::FILE_NAME);
        let rules = RulesSync::new(broadcast::channel(64).0);
        Self {
            _state: state,
            file,
            commands: DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
            rules,
            seen: Arc::default(),
            stream: None,
        }
    }

    /// A daemon whose rule list is `snapshot`, answering as `daemon`.
    fn connect(mut self, daemon: Daemon, snapshot: Vec<Rule>) -> Self {
        self.rules.stage(None, snapshot);
        let (stream, mut rx) = self.commands.open_stream(None);
        let stream_id = stream.id();
        self.commands.on_reply(stream_id, &reply(0, true));
        let commands = self.commands.clone();
        let seen = self.seen.clone();
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                seen.lock().unwrap().push(command.clone());
                commands.on_reply(
                    stream_id,
                    &reply(command.id, matches!(daemon, Daemon::Accept)),
                );
            }
        });
        self.stream = Some((stream, stream_id));
        self
    }

    /// The daemon restarts with rule list `snapshot` (a new HELLO).
    fn resync(&self, snapshot: Vec<Rule>) {
        self.rules.stage(None, snapshot);
        let (_, stream_id) = self.stream.as_ref().unwrap();
        self.commands.on_reply(*stream_id, &reply(0, true));
    }

    fn curated(&self) -> CuratedDefaults {
        let curated = CuratedDefaults::new(
            self.commands.clone(),
            self.rules.cache(),
            broadcast::channel(64).0,
        );
        curated.attach_file(self.file.clone());
        curated
    }

    fn seen(&self) -> Vec<(i32, String)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|n| (n.r#type, n.rules[0].name.clone()))
            .collect()
    }
}

fn turn(curated: &CuratedDefaults, id: &str, on: bool) {
    let routed = curated.try_route(ClientMessage::SetCuratedDefaults {
        ids: vec![id.into()],
        on,
    });
    assert!(routed.is_none(), "the worker takes its own message");
}

fn entry_state(curated: &CuratedDefaults, id: &str) -> CuratedDefaultSummary {
    match curated.message() {
        ServerMessage::SetCuratedDefaults { entries, .. } => {
            entries.into_iter().find(|e| e.id == id).unwrap()
        }
        other => panic!("{other:?}"),
    }
}

fn flatpak_rule() -> Rule {
    entries().iter().find(|e| e.id == FLATPAK).unwrap().rule()
}

#[tokio::test]
async fn nothing_is_sent_until_the_user_turns_an_entry_on() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    curated.reconcile().await;
    assert!(harness.seen().is_empty());
    let ServerMessage::SetCuratedDefaults {
        entries,
        storage,
        unavailable,
    } = curated.message()
    else {
        unreachable!()
    };
    assert!(storage.persistent);
    assert_eq!(unavailable, None);
    assert!(entries
        .iter()
        .all(|e| !e.on && e.status == EntryStatus::Off));
}

#[tokio::test]
async fn an_entry_turned_on_is_installed_and_reported_only_after_the_daemons_ok() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert_eq!(
        harness.seen(),
        [(Action::ChangeRule as i32, FLATPAK_RULE.into())]
    );
    let sent = harness.seen.lock().unwrap()[0].rules[0].clone();
    check_curated_rule(&sent).unwrap();
    let state = entry_state(&curated, FLATPAK);
    assert!(state.on);
    assert_eq!(state.status, EntryStatus::Installed);
    // Saved: on, and the copy that was installed.
    let saved = store::load(&harness.file).unwrap().unwrap();
    assert!(saved.enabled.contains(FLATPAK));
    assert!(saved.installed[FLATPAK].matches(&flatpak_rule()));
}

#[tokio::test]
async fn a_refused_install_is_not_reported_installed() {
    let harness = Harness::new().connect(Daemon::Refuse, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    let state = entry_state(&curated, FLATPAK);
    assert_eq!(state.status, EntryStatus::NotInstalled);
    assert_eq!(
        state.problem.as_deref(),
        Some("The firewall service refused the rule.")
    );
    assert!(store::load(&harness.file)
        .unwrap()
        .unwrap()
        .installed
        .is_empty());
}

#[tokio::test]
async fn a_rule_the_user_deleted_stays_deleted_across_reconciles_and_restarts() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    // The rule is deleted outside Snitchwatch; the daemon restarts.
    harness.resync(Vec::new());
    curated.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "reinstalled: {:?}", harness.seen());
    assert_eq!(
        entry_state(&curated, FLATPAK).status,
        EntryStatus::DeletedOutside
    );
    // The bridge restarts too.
    let restarted = harness.curated();
    restarted.reconcile().await;
    assert_eq!(harness.seen().len(), 1, "reinstalled after a restart");
    assert_eq!(
        entry_state(&restarted, FLATPAK).status,
        EntryStatus::DeletedOutside
    );
    // Turned on again by the user: installed again.
    turn(&restarted, FLATPAK, false);
    turn(&restarted, FLATPAK, true);
    restarted.reconcile().await;
    assert_eq!(harness.seen().len(), 2);
}

#[tokio::test]
async fn turning_an_entry_off_deletes_its_rule() {
    let harness = Harness::new().connect(Daemon::Accept, Vec::new());
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    harness.resync(vec![flatpak_rule()]);
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    assert_eq!(
        harness.seen()[1],
        (Action::DeleteRule as i32, FLATPAK_RULE.into())
    );
    assert_eq!(entry_state(&curated, FLATPAK).status, EntryStatus::Off);
    assert!(store::load(&harness.file)
        .unwrap()
        .unwrap()
        .installed
        .is_empty());
}

#[tokio::test]
async fn nothing_happens_while_the_daemons_rules_are_unknown() {
    let harness = Harness::new();
    let curated = harness.curated();
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    assert!(harness.seen().is_empty());
    assert_eq!(entry_state(&curated, FLATPAK).status, EntryStatus::Waiting);
}

/// The per-user bridge, or one without saved settings (a deletion couldn't
/// be remembered): nothing is installed or removed, and the GUI says why.
#[tokio::test]
async fn an_unavailable_bridge_changes_nothing_and_says_why() {
    let harness = Harness::new().connect(Daemon::Accept, vec![flatpak_rule()]);
    let curated = harness.curated();
    curated.set_unavailable("Needs the system service.");
    turn(&curated, FLATPAK, true);
    curated.reconcile().await;
    turn(&curated, FLATPAK, false);
    curated.reconcile().await;
    assert!(harness.seen().is_empty());
    let ServerMessage::SetCuratedDefaults {
        entries,
        unavailable,
        ..
    } = curated.message()
    else {
        unreachable!()
    };
    assert_eq!(unavailable.as_deref(), Some("Needs the system service."));
    assert!(entries.iter().all(|e| !e.on), "the choice wasn't taken");
    assert!(
        store::load(&harness.file).unwrap().is_none(),
        "nothing saved"
    );
}
