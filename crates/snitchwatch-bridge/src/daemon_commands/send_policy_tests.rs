//! The send point's rule policy (issue #45 PR B): nothing built from a GUI
//! may carry a `lists` operator or a blocklist rule name, and only a
//! [`BlocklistCommand`] (which only the bridge's blocklist code can build)
//! may.

use super::*;
use crate::blocklists::list_dir::{IdComponent, ListDir};
use crate::blocklists::materializer::ListKind;
use snitchwatch_proto::protocol::{Action, Operator, Rule};
use std::path::Path;
use tokio::sync::broadcast;

fn current_stream(commands: &DaemonCommands) -> (StreamRegistration, mpsc::Receiver<Notification>) {
    let (stream, rx) = commands.open_stream(None);
    commands.on_reply(
        stream.id(),
        &NotificationReply {
            id: 0,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        },
    );
    (stream, rx)
}

fn fixture() -> (DaemonCommands, RulesSync) {
    let rules = RulesSync::new(broadcast::channel(16).0);
    (
        DaemonCommands::new(DaemonTransport::Unix, rules.clone()),
        rules,
    )
}

fn change(name: &str, operator: Option<Operator>) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![Rule {
            name: name.into(),
            enabled: true,
            action: "allow".into(),
            duration: "always".into(),
            operator,
            ..Default::default()
        }],
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

fn lists_operator(data: &str) -> Operator {
    Operator {
        r#type: "lists".into(),
        operand: "lists.domains".into(),
        data: data.into(),
        ..Default::default()
    }
}

fn host(data: &str) -> Operator {
    Operator {
        r#type: "simple".into(),
        operand: "dest.host".into(),
        data: data.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_lists_rule_built_outside_the_blocklists_is_refused_at_send() {
    let (commands, _rules) = fixture();
    let (_stream, mut rx) = current_stream(&commands);
    for operator in [
        lists_operator("/etc"),
        lists_operator("/var/lib/snitchwatch/blocklists/ads/domains"),
        Operator {
            r#type: "list".into(),
            list: vec![lists_operator("/etc"), host("x.example")],
            ..Default::default()
        },
    ] {
        assert_eq!(
            commands.send(change("899-x", Some(operator))).err(),
            Some(SendError::RefusedOperator)
        );
    }
    assert_eq!(
        commands.send(change("899-x", None)).err(),
        Some(SendError::RefusedOperator),
        "the daemon rejects a missing operator and applies its default action"
    );
    assert!(rx.try_recv().is_err(), "nothing reached the daemon");
    assert!(commands
        .send(change("899-x", Some(host("x.example"))))
        .is_ok());
}

#[tokio::test]
async fn a_blocklist_rule_name_is_refused_at_send_whatever_the_command() {
    let (commands, _rules) = fixture();
    let (_stream, mut rx) = current_stream(&commands);
    for name in [
        "z00-blocklist:ads:domains",
        "z00-blocklist:ads-0123456789abcdef:ips",
        "900-blocklist:ads:0001-x.example",
    ] {
        assert_eq!(
            commands.send(change(name, Some(host("x.example")))).err(),
            Some(SendError::ReservedName),
            "an allow could replace a blocklist deny: {name}"
        );
        assert_eq!(
            commands.send(delete(name)).err(),
            Some(SendError::ReservedName),
            "{name}"
        );
    }
    assert!(rx.try_recv().is_err(), "nothing reached the daemon");
}

#[tokio::test]
async fn an_internal_blocklist_command_is_sent_and_its_ok_reaches_the_rules_cache() {
    let state = tempfile::tempdir().unwrap();
    let dir = ListDir::open(&state.path().canonicalize().unwrap()).unwrap();
    let (commands, rules) = fixture();
    rules.stage(None, Vec::new());
    let (stream, mut rx) = current_stream(&commands);
    let list = IdComponent::from_id("ads-0123456789abcdef");
    // Review L1: nothing is sent before the sink pins its list root.
    assert_eq!(
        commands
            .send_blocklist(BlocklistCommand::install(&list, ListKind::Domains, &dir))
            .err(),
        Some(SendError::RefusedOperator)
    );
    commands.pin_blocklist_root(dir.root()).unwrap();
    assert!(
        commands
            .pin_blocklist_root(Path::new("/elsewhere/blocklists"))
            .is_err(),
        "the root is pinned once"
    );

    let pending = commands
        .send_blocklist(BlocklistCommand::install(&list, ListKind::Domains, &dir))
        .unwrap();
    let sent = rx.try_recv().unwrap();
    assert_eq!(sent.r#type, Action::ChangeRule as i32);
    let op = sent.rules[0].operator.clone().unwrap();
    assert_eq!(
        (op.r#type.as_str(), op.operand.as_str()),
        ("lists", "lists.domains")
    );
    assert_eq!(
        op.data,
        dir.kind_dir(&list, ListKind::Domains).to_str().unwrap()
    );
    commands.on_reply(
        stream.id(),
        &NotificationReply {
            id: sent.id,
            code: NotificationReplyCode::Ok as i32,
            data: String::new(),
        },
    );
    pending.wait(Duration::from_secs(5)).await.unwrap();
    match rules.cache().lock().unwrap().rules() {
        Some(cached) => {
            assert!(cached.contains_key("z00-blocklist:ads-0123456789abcdef:domains"))
        }
        None => panic!("cache Unknown"),
    }

    let delete = BlocklistCommand::delete("z00-blocklist:ads-0123456789abcdef:domains").unwrap();
    assert!(commands.send_blocklist(delete).is_ok());
    assert_eq!(rx.try_recv().unwrap().r#type, Action::DeleteRule as i32);
}
