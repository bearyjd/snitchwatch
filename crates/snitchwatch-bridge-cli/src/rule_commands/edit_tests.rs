//! A refused edit (P2.1 re-review): the daemon deletes an `always` rule's
//! file before it compiles a temporary replacement (`replaceUserRule`), so
//! an ERROR there leaves the rule applying without its file. The bridge
//! saves the rule again as it was, and says so.

use super::tests::*;
use crate::test_daemon::*;
use serde_json::json;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Action, Notification};

fn changes(seen: &Seen) -> Vec<(String, String)> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|n: &Notification| {
            assert_eq!(n.r#type, Action::ChangeRule as i32);
            (n.rules[0].name.clone(), n.rules[0].duration.clone())
        })
        .collect()
}

fn broken_timed_edit(old: &snitchwatch_proto::protocol::Rule) -> serde_json::Value {
    let mut edited = export_rule(old);
    edited["duration"] = json!("5m");
    edited["operator"]["operands"][1]["data"] = json!("broken.example");
    edited
}

fn reason(outcome: RuleCommandOutcome) -> String {
    match outcome {
        RuleCommandOutcome::Rejected { reason } => reason,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_refused_always_to_timed_edit_saves_the_rule_again() {
    let old = bound("100-x", "deny");
    let mut daemon = daemon(vec![old.clone()]);
    let model = model(std::slice::from_ref(&old));
    model
        .lock()
        .unwrap()
        .uncompilable
        .insert("broken.example".into());
    let seen = run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-x", broken_timed_edit(&old), Some("e1")));
    let reason = reason(result(&mut rx).await);
    assert!(reason.contains("saved the rule again"), "{reason}");
    assert_eq!(
        changes(&seen),
        vec![
            ("100-x".to_string(), "5m".to_string()),
            ("100-x".to_string(), "always".to_string())
        ]
    );
    assert_eq!(files(&model), vec!["100-x"], "the rule's file is back");
    assert_eq!(model.lock().unwrap().memory["100-x"].action, "deny");
}

#[tokio::test]
async fn a_restore_that_fails_says_the_rule_lasts_until_a_restart() {
    let old = bound("100-x", "deny");
    let mut daemon = daemon(vec![old.clone()]);
    let model = model(std::slice::from_ref(&old));
    for value in ["broken.example", "example.com"] {
        model.lock().unwrap().uncompilable.insert(value.into());
    }
    run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-x", broken_timed_edit(&old), Some("e1")));
    let reason = reason(result(&mut rx).await);
    assert!(reason.contains("until the firewall restarts"), "{reason}");
}

/// An edit that keeps `always` loses no file, so nothing is resent.
#[tokio::test]
async fn a_refused_edit_that_keeps_its_file_is_not_resent() {
    let old = bound("100-x", "deny");
    let mut daemon = daemon(vec![old.clone()]);
    let model = model(std::slice::from_ref(&old));
    model
        .lock()
        .unwrap()
        .uncompilable
        .insert("broken.example".into());
    let seen = run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut edited = broken_timed_edit(&old);
    edited["duration"] = json!("always");
    commands.try_route(update("100-x", edited, Some("e1")));
    let reason = reason(result(&mut rx).await);
    assert!(!reason.contains("again"), "{reason}");
    assert_eq!(changes(&seen).len(), 1);
    assert_eq!(files(&model), vec!["100-x"]);
}
