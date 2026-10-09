//! A daemon `ERROR` reaches the rules cache too (tower r12): a refused
//! `DELETE_RULE` means opensnitchd already dropped the rule from memory.

use super::*;
use snitchwatch_proto::protocol::{Action, Operator, Rule};
use tokio::sync::broadcast;

const SHORT: Duration = Duration::from_millis(100);
const LONG: Duration = Duration::from_secs(10);

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

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn refused(id: u64) -> NotificationReply {
    NotificationReply {
        id,
        code: NotificationReplyCode::Error as i32,
        data: "remove /etc/opensnitchd/rules/a.json: operation not permitted".into(),
    }
}

fn hello() -> NotificationReply {
    NotificationReply {
        id: 0,
        code: NotificationReplyCode::Ok as i32,
        data: String::new(),
    }
}

/// A HELLO'd stream whose snapshot is `a` and `b`, and its outbound queue.
fn synced() -> (
    DaemonCommands,
    RulesSync,
    StreamRegistration,
    mpsc::Receiver<Notification>,
) {
    let (tx, _) = broadcast::channel(64);
    let rules = RulesSync::new(tx);
    let commands = DaemonCommands::new(DaemonTransport::Tcp, rules.clone());
    let key = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 1)));
    rules.stage(key, vec![rule("a"), rule("b")]);
    let (stream, outbound) = commands.open_stream(key);
    commands.on_reply(stream.id(), &hello());
    (commands, rules, stream, outbound)
}

fn listed(rules: &RulesSync) -> Vec<String> {
    rules
        .cache()
        .lock()
        .unwrap()
        .rules()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

fn file_left(rules: &RulesSync, name: &str) -> bool {
    rules.cache().lock().unwrap().files_left().contains(name)
}

#[tokio::test]
async fn a_refused_delete_unlists_the_rule_before_its_waiter_hears() {
    let (commands, rules, stream, _outbound) = synced();
    let pending = commands.send(delete("a")).unwrap();
    commands.on_reply(stream.id(), &refused(pending.id()));
    assert_eq!(listed(&rules), vec!["b"], "before the waiter runs");
    assert!(file_left(&rules, "a"));
    assert!(matches!(
        pending.wait(LONG).await,
        Err(CommandError::Rejected(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn a_late_refused_delete_within_the_grace_period_still_unlists_the_rule() {
    let (commands, rules, stream, _outbound) = synced();
    let late = commands.send(delete("a")).unwrap();
    let late_id = late.id();
    assert_eq!(late.wait(SHORT).await, Err(CommandError::Timeout));
    commands.on_reply(stream.id(), &refused(late_id));
    assert_eq!(listed(&rules), vec!["b"]);

    let too_late = commands.send(delete("b")).unwrap();
    let too_late_id = too_late.id();
    assert_eq!(too_late.wait(SHORT).await, Err(CommandError::Timeout));
    tokio::time::advance(LATE_REPLY_GRACE + Duration::from_secs(1)).await;
    commands.on_reply(stream.id(), &refused(too_late_id));
    assert_eq!(listed(&rules), vec!["b"], "too late: ignored");
    assert!(!file_left(&rules, "b"));
}

/// Only the current stream's answers count, refusals included.
#[tokio::test]
async fn a_refusal_from_a_stream_that_is_not_current_is_ignored() {
    let (commands, rules, stream, _outbound) = synced();
    let pending = commands.send(delete("a")).unwrap();
    let (other, _rx) = commands.open_stream(Some(std::net::SocketAddr::from(([127, 0, 0, 1], 2))));
    commands.on_reply(other.id(), &refused(pending.id()));
    assert_eq!(listed(&rules), vec!["a", "b"]);
    drop(stream);
}
