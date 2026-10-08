//! Tests for [`super::DaemonProfileSink`] against a real [`DaemonCommands`]
//! and rules cache, with a scripted daemon stream answering each command.

use super::*;
use crate::cache::rules::RulesSync;
use crate::daemon_commands::{DaemonTransport, StreamRegistration};
use crate::profiles::materializer::materialize_rule;
use crate::profiles::store::ProfileRule;
use snitchwatch_proto::protocol::{Action, Notification, NotificationReply, NotificationReplyCode};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::broadcast;

#[derive(Clone)]
enum Daemon {
    Accept,
    Refuse(&'static str),
    Silent,
}

struct Harness {
    commands: DaemonCommands,
    rules: RulesSync,
    seen: Arc<StdMutex<Vec<Notification>>>,
    _stream: Option<StreamRegistration>,
}

impl Harness {
    fn new() -> Self {
        let rules = RulesSync::new(broadcast::channel(64).0);
        Self {
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
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                seen.lock().unwrap().push(command.clone());
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

    fn sink(&self) -> DaemonProfileSink {
        DaemonProfileSink::new(self.commands.clone(), self.rules.cache())
            .with_timeout(Duration::from_millis(300))
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

fn wanted(profile: &str, id: &str, seq: usize, host: &str) -> Rule {
    let rule = ProfileRule {
        id: id.into(),
        action: "deny".into(),
        operand: "dest.host".into(),
        data: host.into(),
        operator: None,
    };
    materialize_rule(profile, &rule, seq).unwrap()
}

const CHANGE: i32 = Action::ChangeRule as i32;
const DELETE: i32 = Action::DeleteRule as i32;

fn installed(outcomes: &[Enforcement]) -> bool {
    outcomes
        .iter()
        .all(|o| matches!(o, Enforcement::RuleInstalled { .. }))
}

#[tokio::test]
async fn wanted_rules_are_installed_only_after_the_daemons_ok() {
    let h = Harness::new().connect(Daemon::Accept, vec![]);
    let rules = vec![
        wanted("home", "r1", 0, "a.example"),
        wanted("home", "r2", 1, "b.example"),
    ];
    let outcomes = h.sink().apply(&rules).await;
    assert!(installed(&outcomes), "{outcomes:?}");
    assert_eq!(
        h.seen(),
        vec![
            (CHANGE, rules[0].name.clone()),
            (CHANGE, rules[1].name.clone())
        ]
    );
}

/// What the daemon echoes in its next snapshot (a list operand spelled
/// `list`, a non-case-sensitive pattern lowercased) counts as in place, so
/// a reconcile after every snapshot sends nothing.
#[tokio::test]
async fn a_rule_already_in_the_snapshot_is_not_resent() {
    let rule = ProfileRule {
        id: "r1".into(),
        action: "allow".into(),
        operand: String::new(),
        data: String::new(),
        operator: Some(serde_json::json!({ "type": "list", "operands": [
            { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl",
              "sensitive": true },
            { "type": "regexp", "operand": "dest.host", "data": "^Example\\.COM$" },
        ] })),
    };
    let wanted = materialize_rule("home", &rule, 0).unwrap();
    let mut echoed = wanted.clone();
    let op = echoed.operator.as_mut().unwrap();
    op.operand = "list".into();
    op.list[1].data = op.list[1].data.to_lowercase();
    echoed.created = 1_700_000_000;
    let h = Harness::new().connect(Daemon::Accept, vec![echoed]);
    let outcomes = h.sink().apply(std::slice::from_ref(&wanted)).await;
    assert!(installed(&outcomes), "{outcomes:?}");
    assert!(h.seen().is_empty(), "resent: {:?}", h.seen());
}

#[tokio::test]
async fn a_daemon_refusal_is_shown_as_plain_text() {
    let h = Harness::new().connect(Daemon::Refuse("bad \u{202e}rule"), vec![]);
    let outcomes = h
        .sink()
        .apply(&[wanted("home", "r1", 0, "a.example")])
        .await;
    match &outcomes[0] {
        Enforcement::NotEnforced { reason } => {
            assert!(reason.contains("bad rule"), "{reason}");
            assert!(!reason.contains('\u{202e}'), "{reason}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_silent_daemon_stops_the_pass_and_deletes_nothing() {
    let stray = wanted("old", "x", 0, "x.example");
    let h = Harness::new().connect(Daemon::Silent, vec![stray]);
    let rules = vec![
        wanted("home", "r1", 0, "a.example"),
        wanted("home", "r2", 1, "b.example"),
    ];
    let outcomes = h.sink().apply(&rules).await;
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, Enforcement::Unconfirmed { .. })),
        "{outcomes:?}"
    );
    assert_eq!(
        h.seen().len(),
        1,
        "stopped after the first unanswered command"
    );
}

#[tokio::test]
async fn nothing_is_sent_while_the_daemons_rules_are_unknown() {
    let h = Harness::new();
    let outcomes = h
        .sink()
        .apply(&[wanted("home", "r1", 0, "a.example")])
        .await;
    assert!(
        matches!(&outcomes[0], Enforcement::Unconfirmed { .. }),
        "{outcomes:?}"
    );
    assert!(h.seen().is_empty());
}

/// Only rules the bridge made are deleted: the prefix and the profile tag.
#[tokio::test]
async fn the_purge_deletes_only_the_bridges_other_profile_rules() {
    let keep = wanted("home", "r1", 0, "a.example");
    let stray = wanted("old", "x", 0, "x.example");
    let untagged = Rule {
        name: "850-profile:someone:0000-y".into(),
        description: String::new(),
        ..stray.clone()
    };
    let user_rule = Rule {
        name: "899-user".into(),
        description: String::new(),
        ..stray.clone()
    };
    let h = Harness::new().connect(
        Daemon::Accept,
        vec![keep.clone(), stray.clone(), untagged, user_rule],
    );
    let outcomes = h.sink().apply(std::slice::from_ref(&keep)).await;
    assert!(installed(&outcomes));
    assert_eq!(h.seen(), vec![(DELETE, stray.name)]);
}

#[tokio::test]
async fn deactivating_deletes_every_rule_the_bridge_made() {
    let a = wanted("home", "r1", 0, "a.example");
    let b = wanted("home", "r2", 1, "b.example");
    let h = Harness::new().connect(Daemon::Accept, vec![a.clone(), b.clone()]);
    assert!(h.sink().apply(&[]).await.is_empty());
    let mut deleted: Vec<String> = h.seen().into_iter().map(|(_, name)| name).collect();
    deleted.sort();
    assert_eq!(deleted, vec![a.name, b.name]);
}

#[tokio::test]
async fn the_noop_sink_says_why_nothing_is_installed() {
    let sink = NoopProfileRuleSink::new("per-user mode");
    let outcomes = sink.apply(&[wanted("home", "r1", 0, "a.example")]).await;
    assert_eq!(
        outcomes,
        vec![Enforcement::NotEnforced {
            reason: "per-user mode".into()
        }]
    );
    assert_eq!(sink.not_applied_reason(), Some("per-user mode"));
}
