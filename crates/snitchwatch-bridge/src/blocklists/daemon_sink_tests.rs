//! Tests for [`super::DaemonRuleSink`] against a real [`DaemonCommands`] and
//! rules cache, with a scripted daemon stream answering each command.

use super::*;
use crate::cache::rules::RulesSync;
use crate::daemon_commands::{DaemonTransport, StreamRegistration};
use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Operator,
};
use std::path::Path;
use std::sync::Mutex as StdMutex;
use tokio::sync::broadcast;

pub(in crate::blocklists) const ADS: &str = "ads-0123456789abcdef";

#[derive(Clone)]
pub(in crate::blocklists) enum Daemon {
    Accept,
    Refuse(&'static str),
    /// Refuses `CHANGE_RULE` (an install), accepts `DELETE_RULE`.
    RefuseChange(&'static str),
    /// Refuses `DELETE_RULE`, accepts `CHANGE_RULE`.
    RefuseDelete(&'static str),
    Silent,
}

/// What the scripted daemon saw: the command, and whether the rule's list
/// file (CHANGE) or list directory (DELETE) existed when it arrived.
#[derive(Clone, Debug)]
pub(in crate::blocklists) struct Seen {
    pub(in crate::blocklists) command: Notification,
    pub(in crate::blocklists) path_existed: bool,
}

pub(in crate::blocklists) struct Harness {
    _state: tempfile::TempDir,
    pub(in crate::blocklists) dir: ListDir,
    pub(in crate::blocklists) commands: DaemonCommands,
    pub(in crate::blocklists) rules: RulesSync,
    seen: Arc<StdMutex<Vec<Seen>>>,
    /// How the scripted daemon answers; [`Harness::set_daemon`] changes it.
    mode: Arc<StdMutex<Daemon>>,
    _stream: Option<StreamRegistration>,
}

impl Harness {
    pub(in crate::blocklists) fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        let dir = ListDir::open(&state.path().canonicalize().unwrap()).unwrap();
        let rules = RulesSync::new(broadcast::channel(64).0);
        Self {
            _state: state,
            dir,
            commands: DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
            rules,
            seen: Arc::default(),
            mode: Arc::new(StdMutex::new(Daemon::Accept)),
            _stream: None,
        }
    }

    /// Change how the connected daemon answers from now on.
    pub(in crate::blocklists) fn set_daemon(&self, daemon: Daemon) {
        *self.mode.lock().unwrap() = daemon;
    }

    /// Connect a daemon whose rule snapshot is `snapshot`.
    pub(in crate::blocklists) fn connect(mut self, daemon: Daemon, snapshot: Vec<Rule>) -> Self {
        self.rules.stage(None, snapshot);
        let (stream, mut rx) = self.commands.open_stream(None);
        let stream_id = stream.id();
        self.commands.on_reply(stream_id, &reply(0, Ok(())));
        let commands = self.commands.clone();
        let seen = self.seen.clone();
        *self.mode.lock().unwrap() = daemon;
        let mode = self.mode.clone();
        let root = self.dir.root().to_path_buf();
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                let rule = &command.rules[0];
                let path_existed = match &rule.operator {
                    Some(op) => {
                        let kind = ListKind::from_operand(&op.operand).unwrap();
                        Path::new(&op.data).join(kind.file_name()).is_file()
                    }
                    None => list_of_rule_name(&rule.name).is_some_and(|l| root.join(l).exists()),
                };
                seen.lock().unwrap().push(Seen {
                    command: command.clone(),
                    path_existed,
                });
                let answer = mode.lock().unwrap().clone();
                match answer {
                    Daemon::Accept => commands.on_reply(stream_id, &reply(command.id, Ok(()))),
                    Daemon::Refuse(text) => {
                        commands.on_reply(stream_id, &reply(command.id, Err(text)))
                    }
                    Daemon::RefuseChange(text) if command.r#type == Action::ChangeRule as i32 => {
                        commands.on_reply(stream_id, &reply(command.id, Err(text)))
                    }
                    Daemon::RefuseChange(_) => {
                        commands.on_reply(stream_id, &reply(command.id, Ok(())))
                    }
                    Daemon::RefuseDelete(text) if command.r#type == Action::DeleteRule as i32 => {
                        commands.on_reply(stream_id, &reply(command.id, Err(text)))
                    }
                    Daemon::RefuseDelete(_) => {
                        commands.on_reply(stream_id, &reply(command.id, Ok(())))
                    }
                    Daemon::Silent => false,
                };
            }
        });
        self._stream = Some(stream);
        self
    }

    /// A new bridge run over the same list directory: fresh commands, rules
    /// cache and confirmations.
    pub(in crate::blocklists) fn restart(self) -> Self {
        let rules = RulesSync::new(broadcast::channel(64).0);
        Self {
            _state: self._state,
            dir: self.dir,
            commands: DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
            rules,
            seen: Arc::default(),
            mode: self.mode,
            _stream: None,
        }
    }

    /// The rule the bridge installs for `id`'s `kind`.
    pub(in crate::blocklists) fn bridge_rule(&self, id: &str, kind: ListKind) -> Rule {
        let list = IdComponent::from_id(id);
        crate::blocklists::materializer::materialize_list_rule(
            &list,
            kind,
            &self.dir.kind_dir(&list, kind),
        )
        .into()
    }

    pub(in crate::blocklists) fn sink(&self) -> DaemonRuleSink {
        DaemonRuleSink::new(self.dir.clone(), self.commands.clone(), self.rules.cache())
            .with_timeout(Duration::from_millis(300))
    }

    pub(in crate::blocklists) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(in crate::blocklists) fn cached(&self) -> Option<Vec<String>> {
        self.rules
            .cache()
            .lock()
            .unwrap()
            .rules()
            .map(|rules| rules.keys().cloned().collect())
    }
}

fn reply(id: u64, outcome: Result<(), &str>) -> NotificationReply {
    NotificationReply {
        id,
        code: match outcome {
            Ok(()) => NotificationReplyCode::Ok as i32,
            Err(_) => NotificationReplyCode::Error as i32,
        },
        data: outcome.err().unwrap_or_default().to_string(),
    }
}

pub(in crate::blocklists) fn hosts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

pub(in crate::blocklists) fn kind_of(n: &Notification) -> (i32, String) {
    (n.r#type, n.rules[0].name.clone())
}

pub(in crate::blocklists) fn change(name: &str) -> (i32, String) {
    (Action::ChangeRule as i32, name.to_string())
}

pub(in crate::blocklists) fn delete(name: &str) -> (i32, String) {
    (Action::DeleteRule as i32, name.to_string())
}

/// A per-host rule as earlier builds named and tagged them.
pub(in crate::blocklists) fn legacy_rule(name: &str) -> Rule {
    Rule {
        description: r#"{"snitchwatch":{"source":"blocklist","list_id":"x","entry":"x.example"}}"#
            .into(),
        ..user_rule(name)
    }
}

pub(in crate::blocklists) fn user_rule(name: &str) -> Rule {
    Rule {
        name: name.into(),
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

#[tokio::test]
async fn files_are_written_before_each_rule_and_installed_only_after_the_daemons_ok() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["ads.example", "203.0.113.7"]))
        .await
        .unwrap();
    let seen = h.seen();
    let kinds: Vec<_> = seen.iter().map(|s| kind_of(&s.command)).collect();
    assert_eq!(
        kinds,
        vec![
            change(&format!("z00-blocklist:{ADS}:domains")),
            change(&format!("z00-blocklist:{ADS}:ips")),
        ]
    );
    assert!(
        seen.iter().all(|s| s.path_existed),
        "a rule went out before its file"
    );
    for s in &seen {
        let op = s.command.rules[0].operator.as_ref().unwrap();
        assert!(Path::new(&op.data).starts_with(h.dir.root()), "{}", op.data);
    }
    let cached = h.cached().unwrap();
    assert!(cached.contains(&format!("z00-blocklist:{ADS}:domains")));
    assert!(sink.is_current(ADS));
}

#[tokio::test]
async fn no_reply_is_not_installed_and_marks_the_daemon_unavailable() {
    let h = Harness::new().connect(Daemon::Silent, Vec::new());
    let err = h
        .sink()
        .replace_blocklist_rules(ADS, hosts(&["ads.example"]))
        .await
        .unwrap_err();
    assert!(err.daemon_unavailable, "{err:?}");
    assert!(err.reason.contains("didn't answer"), "{}", err.reason);
    assert!(!h.sink().is_current(ADS));
}

#[tokio::test]
async fn a_refusing_daemon_is_not_enforced_with_its_reason_in_plain_text() {
    let h = Harness::new().connect(
        Daemon::Refuse("lists operators are not accepted\u{202e} from the UI\n"),
        Vec::new(),
    );
    let err = h
        .sink()
        .replace_blocklist_rules(ADS, hosts(&["ads.example"]))
        .await
        .unwrap_err();
    assert!(!err.daemon_unavailable);
    assert!(
        err.reason.starts_with("The firewall service refused"),
        "{}",
        err.reason
    );
    assert!(
        err.reason.contains("lists operators are not accepted"),
        "{}",
        err.reason
    );
    assert!(
        !err.reason
            .chars()
            .any(|c| c.is_control() || c == '\u{202e}'),
        "{:?}",
        err.reason
    );
}

#[tokio::test]
async fn without_a_daemon_rule_list_nothing_is_sent() {
    let h = Harness::new();
    let sink = h.sink();
    let err = sink
        .replace_blocklist_rules(ADS, hosts(&["ads.example"]))
        .await
        .unwrap_err();
    assert!(err.daemon_unavailable);
    assert!(err.reason.contains("isn't connected"), "{}", err.reason);
    assert!(!sink.daemon_rules_known());
    assert!(h.seen().is_empty());
}

#[tokio::test]
async fn a_list_that_cant_be_written_sends_nothing() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), h.dir.root().join(ADS)).unwrap();
    let err = h
        .sink()
        .replace_blocklist_rules(ADS, hosts(&["ads.example"]))
        .await
        .unwrap_err();
    assert!(
        err.reason.starts_with("Couldn't save the list"),
        "{}",
        err.reason
    );
    assert!(h.seen().is_empty());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_unchanged_refresh_rewrites_the_file_without_resending_the_rule() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    sink.replace_blocklist_rules(ADS, hosts(&["b.example"]))
        .await
        .unwrap();
    assert_eq!(h.seen().len(), 1, "the daemon re-reads the file by itself");
    let file = h
        .dir
        .kind_dir(&IdComponent::from_id(ADS), ListKind::Domains)
        .join("domains.list");
    assert_eq!(
        std::fs::read_to_string(file).unwrap(),
        "0.0.0.0 b.example\n"
    );
}

#[tokio::test]
async fn a_list_that_loses_its_ips_deletes_that_rule_then_its_directory() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example", "203.0.113.7"]))
        .await
        .unwrap();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let seen = h.seen();
    let last = seen.last().unwrap();
    assert_eq!(
        kind_of(&last.command),
        delete(&format!("z00-blocklist:{ADS}:ips"))
    );
    assert!(!h
        .dir
        .kind_dir(&IdComponent::from_id(ADS), ListKind::Ips)
        .exists());
    assert!(h.cached().unwrap().iter().all(|n| !n.ends_with(":ips")));
}

#[tokio::test]
async fn legacy_rules_of_the_list_are_deleted_and_user_rules_kept() {
    let snapshot = vec![
        legacy_rule(&format!("z00-blocklist:{ADS}:0001-x.example")),
        legacy_rule(&format!("900-blocklist:{ADS}:0002-y.example")),
        // Under the list's prefix, but not made by Snitchwatch: left alone.
        user_rule(&format!("z00-blocklist:{ADS}:0003-z.example")),
        user_rule("899-user"),
        user_rule("z00-blocklist:other:domains"),
    ];
    let h = Harness::new().connect(Daemon::Accept, snapshot);
    h.sink()
        .replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let deleted: Vec<_> = h
        .seen()
        .iter()
        .map(|s| kind_of(&s.command))
        .filter(|(t, _)| *t == Action::DeleteRule as i32)
        .collect();
    assert_eq!(
        deleted,
        vec![
            delete(&format!("900-blocklist:{ADS}:0002-y.example")),
            delete(&format!("z00-blocklist:{ADS}:0001-x.example")),
        ]
    );
}

#[tokio::test]
async fn an_empty_list_installs_nothing_and_removes_what_it_had() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let err = sink
        .replace_blocklist_rules(ADS, hosts(&["localhost", "127.0.0.1"]))
        .await
        .unwrap_err();
    assert_eq!(err.reason, crate::blocklists::NO_HOSTS_REASON);
    assert_eq!(
        kind_of(&h.seen().last().unwrap().command),
        delete(&format!("z00-blocklist:{ADS}:domains"))
    );
    assert!(!h
        .dir
        .kind_dir(&IdComponent::from_id(ADS), ListKind::Domains)
        .exists());
}

#[tokio::test]
async fn unsubscribing_deletes_the_rules_first_then_the_directory() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example", "203.0.113.7"]))
        .await
        .unwrap();
    let installs = h.seen().len();
    sink.remove_blocklist_rules(ADS).await.unwrap();
    let deletes: Vec<Seen> = h.seen().split_off(installs);
    let names: Vec<_> = deletes.iter().map(|s| kind_of(&s.command)).collect();
    assert_eq!(
        names,
        vec![
            delete(&format!("z00-blocklist:{ADS}:domains")),
            delete(&format!("z00-blocklist:{ADS}:ips")),
        ]
    );
    assert!(
        deletes.iter().all(|s| s.path_existed),
        "the directory went before its rules"
    );
    assert!(!h.dir.list_dir(&IdComponent::from_id(ADS)).exists());
    assert!(!sink.is_current(ADS));
}

#[tokio::test]
async fn orphaned_rules_and_directories_are_removed_subscribed_ones_kept() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule("gone", ListKind::Domains),
        user_rule("899-user"),
    ];
    let h = h.connect(Daemon::Accept, snapshot);
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    h.dir
        .write_list(
            &IdComponent::from_id("gone"),
            ListKind::Domains,
            &hosts(&["g.example"]),
        )
        .unwrap();
    sink.remove_orphans(&[ADS.to_string()]).await;
    assert_eq!(
        kind_of(&h.seen().last().unwrap().command),
        delete("z00-blocklist:gone:domains")
    );
    assert_eq!(h.dir.lists().unwrap(), vec![IdComponent::from_id(ADS)]);
    let cached = h.cached().unwrap();
    assert!(cached.contains(&"899-user".to_string()));
    assert!(!cached.contains(&"z00-blocklist:gone:domains".to_string()));
}

#[tokio::test]
async fn a_hostile_stored_id_stays_inside_the_lists_directory() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let raw = "../../etc/cron.d/x:y";
    h.sink()
        .replace_blocklist_rules(raw, hosts(&["a.example"]))
        .await
        .unwrap();
    let seen = h.seen();
    let rule = &seen[0].command.rules[0];
    assert!(crate::rule_name::validate_rule_name(&rule.name).is_ok());
    let data = Path::new(&rule.operator.as_ref().unwrap().data).to_path_buf();
    assert_eq!(data.parent().unwrap().parent().unwrap(), h.dir.root());
    assert_eq!(h.dir.lists().unwrap(), vec![IdComponent::from_id(raw)]);
}

#[tokio::test]
async fn is_current_needs_the_cached_rule_and_its_file() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    assert!(sink.is_current(ADS));
    let list = IdComponent::from_id(ADS);
    std::fs::remove_file(
        h.dir
            .kind_dir(&list, ListKind::Domains)
            .join("domains.list"),
    )
    .unwrap();
    assert!(!sink.is_current(ADS), "a missing file must be rewritten");
}

/// Review L3: the orphan purge deletes only rules Snitchwatch made (its
/// `lists` shape, or the blocklist description tag), whatever their name.
#[tokio::test]
async fn the_orphan_purge_leaves_rules_snitchwatch_did_not_make() {
    let h = Harness::new();
    let snapshot = vec![
        h.bridge_rule("gone", ListKind::Domains),
        legacy_rule("900-blocklist:gone:0001-x.example"),
        user_rule("z00-blocklist:foreign:domains"),
    ];
    let h = h.connect(Daemon::Accept, snapshot);
    h.sink().remove_orphans(&[]).await;
    let deleted: Vec<_> = h.seen().iter().map(|s| kind_of(&s.command)).collect();
    assert_eq!(
        deleted,
        vec![
            delete("900-blocklist:gone:0001-x.example"),
            delete("z00-blocklist:gone:domains"),
        ]
    );
    assert!(h
        .cached()
        .unwrap()
        .contains(&"z00-blocklist:foreign:domains".to_string()));
}

/// The fetch rule the system image ships is not the bridge's to purge,
/// in either form the daemon may report it.
#[tokio::test]
async fn the_orphan_purge_leaves_the_packaged_fetch_rule() {
    use crate::rule_wire::test_helpers::{packaged_fetch_rule, PACKAGED_FETCH_RULE_NAME};
    for uid in [None, Some("987")] {
        let h = Harness::new();
        let snapshot = vec![
            h.bridge_rule("gone", ListKind::Domains),
            packaged_fetch_rule(uid),
        ];
        let h = h.connect(Daemon::Accept, snapshot);
        h.sink().remove_orphans(&[]).await;
        let sent: Vec<_> = h.seen().iter().map(|s| kind_of(&s.command)).collect();
        assert_eq!(sent, vec![delete("z00-blocklist:gone:domains")]);
        assert!(h
            .cached()
            .unwrap()
            .contains(&PACKAGED_FETCH_RULE_NAME.to_string()));
    }
}

/// Review H1: after a bridge restart, rules the daemon's committed snapshot
/// already holds unchanged, over files that are unchanged, are neither
/// resent nor rewritten (no restart storm of `CHANGE_RULE`s and re-reads).
#[tokio::test]
async fn a_restart_with_the_rules_in_the_snapshot_resends_and_rewrites_nothing() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let hosts_now = hosts(&["a.example", "203.0.113.7"]);
    h.sink()
        .replace_blocklist_rules(ADS, hosts_now.clone())
        .await
        .unwrap();
    let list = IdComponent::from_id(ADS);
    let files: Vec<_> = ListKind::ALL
        .into_iter()
        .map(|kind| h.dir.kind_dir(&list, kind).join(kind.file_name()))
        .collect();
    let inodes = |files: &[std::path::PathBuf]| -> Vec<u64> {
        use std::os::unix::fs::MetadataExt;
        files
            .iter()
            .map(|f| std::fs::metadata(f).unwrap().ino())
            .collect()
    };
    let before = inodes(&files);
    let snapshot: Vec<Rule> = ListKind::ALL
        .into_iter()
        .map(|kind| h.bridge_rule(ADS, kind))
        .collect();

    let h = h.restart().connect(Daemon::Accept, snapshot);
    let sink = h.sink();
    assert!(sink.is_current(ADS), "the daemon already holds both rules");
    sink.replace_blocklist_rules(ADS, hosts_now).await.unwrap();
    assert!(h.seen().is_empty(), "{:?}", h.seen());
    assert_eq!(
        inodes(&files),
        before,
        "unchanged list files were rewritten"
    );
}

/// Review L1: a rule whose directory isn't under the root the sink pinned
/// never leaves the bridge.
#[tokio::test]
async fn a_rule_outside_the_pinned_list_root_is_refused() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let _sink = h.sink();
    let other_state = tempfile::tempdir().unwrap();
    let other = ListDir::open(&other_state.path().canonicalize().unwrap()).unwrap();
    let command = BlocklistCommand::install(&IdComponent::from_id(ADS), ListKind::Domains, &other);
    assert_eq!(
        h.commands.send_blocklist(command).err(),
        Some(SendError::RefusedOperator)
    );
    assert!(h.seen().is_empty());
}

/// A rule found in place in the snapshot counts as confirmed: if the
/// daemon's list later goes Unknown (it restarted), a refresh still reports
/// the list installed instead of "rules unknown".
#[tokio::test]
async fn a_rule_found_in_the_snapshot_stays_confirmed_while_the_list_is_unknown() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    h.sink()
        .replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let snapshot = vec![h.bridge_rule(ADS, ListKind::Domains)];
    let h = h.restart().connect(Daemon::Accept, snapshot);
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    h.rules.withdraw();
    sink.replace_blocklist_rules(ADS, hosts(&["b.example"]))
        .await
        .unwrap();
    assert!(h.seen().is_empty(), "{:?}", h.seen());
}

/// Re-review N1: a list file that is wrong on disk is repaired from the
/// store on the first pass of a run, without resending a rule the daemon
/// already holds; afterwards only the rule is resent when it goes missing.
#[tokio::test]
async fn a_wrong_list_file_is_repaired_and_a_missing_rule_resent_without_rewriting() {
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    h.sink()
        .replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let list = IdComponent::from_id(ADS);
    let file = h
        .dir
        .kind_dir(&list, ListKind::Domains)
        .join("domains.list");
    std::fs::write(&file, "0.0.0.0 tampered.example\n").unwrap();
    let snapshot = vec![h.bridge_rule(ADS, ListKind::Domains)];

    let h = h.restart().connect(Daemon::Accept, snapshot);
    let sink = h.sink();
    assert!(
        !sink.files_verified(ADS),
        "a new run hasn't checked the files"
    );
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "0.0.0.0 a.example\n"
    );
    assert!(h.seen().is_empty(), "the rule in place was resent");
    assert!(sink.files_verified(ADS));
}

/// Code re-review: with the list's files already checked in this run, a
/// rule the daemon no longer holds is resent alone; the file is neither
/// re-read from the store nor rewritten.
#[tokio::test]
async fn a_missing_rule_is_resent_without_touching_its_checked_file() {
    use std::os::unix::fs::MetadataExt;
    let h = Harness::new().connect(Daemon::Accept, Vec::new());
    let sink = h.sink();
    sink.replace_blocklist_rules(ADS, hosts(&["a.example"]))
        .await
        .unwrap();
    let list = IdComponent::from_id(ADS);
    let file = h
        .dir
        .kind_dir(&list, ListKind::Domains)
        .join("domains.list");
    let inode = std::fs::metadata(&file).unwrap().ino();
    h.rules
        .cache()
        .lock()
        .unwrap()
        .remove(&format!("z00-blocklist:{ADS}:domains"));
    assert!(sink.files_verified(ADS));
    assert!(!sink.is_current(ADS));
    sink.reinstall_blocklist_rules(ADS).await.unwrap();
    let seen = h.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        kind_of(&seen[1].command),
        change(&format!("z00-blocklist:{ADS}:domains"))
    );
    assert_eq!(std::fs::metadata(&file).unwrap().ino(), inode);
    assert!(sink.is_current(ADS));
}
