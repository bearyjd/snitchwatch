//! Tests for [`super::DaemonRuleSink`] against a real [`DaemonCommands`] and
//! rules cache, with a scripted daemon stream answering each command.

use super::*;
use crate::cache::rules::{RulesCache, RulesSync};
use crate::daemon_commands::{DaemonTransport, StreamRegistration};
use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Operator,
};
use std::path::Path;
use std::sync::Mutex as StdMutex;
use tokio::sync::broadcast;

const ADS: &str = "ads-0123456789abcdef";

#[derive(Clone)]
enum Daemon {
    Accept,
    Refuse(&'static str),
    Silent,
}

/// What the scripted daemon saw: the command, and whether the rule's list
/// file (CHANGE) or list directory (DELETE) existed when it arrived.
#[derive(Clone, Debug)]
struct Seen {
    command: Notification,
    path_existed: bool,
}

struct Harness {
    _state: tempfile::TempDir,
    dir: ListDir,
    commands: DaemonCommands,
    rules: RulesSync,
    seen: Arc<StdMutex<Vec<Seen>>>,
    _stream: Option<StreamRegistration>,
}

impl Harness {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        let dir = ListDir::open(&state.path().canonicalize().unwrap()).unwrap();
        let rules = RulesSync::new(broadcast::channel(64).0);
        Self {
            _state: state,
            dir,
            commands: DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
            rules,
            seen: Arc::default(),
            _stream: None,
        }
    }

    /// Connect a daemon whose rule snapshot is `snapshot`.
    fn connect(mut self, daemon: Daemon, snapshot: Vec<Rule>) -> Self {
        self.rules.stage(None, snapshot);
        let (stream, mut rx) = self.commands.open_stream(None);
        let stream_id = stream.id();
        self.commands.on_reply(stream_id, &reply(0, Ok(())));
        let commands = self.commands.clone();
        let seen = self.seen.clone();
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
                match &daemon {
                    Daemon::Accept => commands.on_reply(stream_id, &reply(command.id, Ok(()))),
                    Daemon::Refuse(text) => {
                        commands.on_reply(stream_id, &reply(command.id, Err(text)))
                    }
                    Daemon::Silent => false,
                };
            }
        });
        self._stream = Some(stream);
        self
    }

    fn sink(&self) -> DaemonRuleSink {
        DaemonRuleSink::new(self.dir.clone(), self.commands.clone(), self.rules.cache())
            .with_timeout(Duration::from_millis(300))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn cached(&self) -> Option<Vec<String>> {
        match &*self.rules.cache().lock().unwrap() {
            RulesCache::Unknown => None,
            RulesCache::Synced(rules) => Some(rules.keys().cloned().collect()),
        }
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

fn hosts(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn kind_of(n: &Notification) -> (i32, String) {
    (n.r#type, n.rules[0].name.clone())
}

fn change(name: &str) -> (i32, String) {
    (Action::ChangeRule as i32, name.to_string())
}

fn delete(name: &str) -> (i32, String) {
    (Action::DeleteRule as i32, name.to_string())
}

fn user_rule(name: &str) -> Rule {
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
        user_rule(&format!("z00-blocklist:{ADS}:0001-x.example")),
        user_rule(&format!("900-blocklist:{ADS}:0002-y.example")),
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
    let snapshot = vec![
        user_rule("z00-blocklist:gone:domains"),
        user_rule("899-user"),
    ];
    let h = Harness::new().connect(Daemon::Accept, snapshot);
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
