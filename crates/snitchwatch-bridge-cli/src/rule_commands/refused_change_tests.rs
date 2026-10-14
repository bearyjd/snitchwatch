//! A refused `CHANGE_RULE` from the editor or the Rules page, by what stock
//! opensnitchd v1.8.0 left (`replaceUserRule`, then `Save` for `always`):
//! a disabled `always` rule applies, not saved (#153); an enabled `always`
//! rule may or may not apply (#120 item 7); a temporary rule doesn't.

use super::tests::*;
use crate::test_daemon::*;
use serde_json::json;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::Rule;

const SAVE_FAILED: &str = "open: operation not permitted";

fn off(rule: Rule) -> Rule {
    Rule {
        enabled: false,
        ..rule
    }
}

/// Sends `message` to a daemon holding `rules` that refuses everything, and
/// returns the result and the cached rule `name` afterwards.
async fn refused_result(
    rules: Vec<Rule>,
    message: snitchwatch_bridge::ws_messages::ClientMessage,
    name: &str,
) -> (RuleCommandOutcome, Option<Rule>) {
    let mut daemon = daemon(rules);
    respond(&mut daemon, |_| Some((false, SAVE_FAILED.into())));
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(message);
    let outcome = result(&mut rx).await;
    let cached = daemon
        .cache
        .lock()
        .unwrap()
        .rules()
        .unwrap()
        .get(name)
        .cloned();
    (outcome, cached)
}

fn note(outcome: &RuleCommandOutcome) -> &str {
    match outcome {
        RuleCommandOutcome::OkWithNote { note } => note,
        other => panic!("expected OkWithNote, got {other:?}"),
    }
}

fn unsure(outcome: &RuleCommandOutcome) -> &str {
    match outcome {
        RuleCommandOutcome::Unsure { reason } => reason,
        other => panic!("expected Unsure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_add_of_a_disabled_saved_rule_applies_but_is_not_saved() {
    let wire = export_rule(&off(bound("100-x", "deny")));
    let (outcome, cached) = refused_result(Vec::new(), add(wire, Some("a")), "100-x").await;
    let note = note(&outcome);
    assert!(note.contains("couldn't save"), "{note}");
    assert!(note.contains(SAVE_FAILED), "{note}");
    assert!(!cached.expect("the daemon holds it").enabled);
}

#[tokio::test]
async fn a_refused_edit_of_a_disabled_saved_rule_applies_but_is_not_saved() {
    let old = off(bound("100-x", "deny"));
    let mut edited = export_rule(&old);
    edited["action"] = json!("reject");
    let (outcome, cached) =
        refused_result(vec![old], update("100-x", edited, Some("e")), "100-x").await;
    let note = note(&outcome);
    assert!(note.contains("couldn't save"), "{note}");
    assert_eq!(cached.expect("the daemon holds it").action, "reject");
}

#[tokio::test]
async fn a_refused_add_of_an_enabled_saved_rule_may_or_may_not_have_applied() {
    let wire = export_rule(&bound("100-x", "deny"));
    let (outcome, _) = refused_result(Vec::new(), add(wire, Some("a")), "100-x").await;
    let reason = unsure(&outcome);
    assert!(reason.contains("may or may not have applied"), "{reason}");
    assert!(reason.contains(SAVE_FAILED), "{reason}");
}

#[tokio::test]
async fn a_refused_edit_of_an_enabled_saved_rule_may_or_may_not_have_applied() {
    let old = bound("100-x", "deny");
    let mut edited = export_rule(&old);
    edited["action"] = json!("reject");
    let (outcome, cached) =
        refused_result(vec![old], update("100-x", edited, Some("e")), "100-x").await;
    assert!(unsure(&outcome).contains("may or may not have applied"));
    assert_eq!(
        cached.expect("still listed").action,
        "deny",
        "the list stays as it was"
    );
}

#[tokio::test]
async fn a_refused_turn_on_of_a_saved_rule_may_or_may_not_have_applied() {
    let old = off(bound("100-x", "deny"));
    let mut on = export_rule(&old);
    on["enabled"] = json!(true);
    let (outcome, cached) =
        refused_result(vec![old], update("100-x", on, Some("t")), "100-x").await;
    assert!(unsure(&outcome).contains("may or may not have applied"));
    assert!(!cached.expect("still listed").enabled);
}

/// A temporary rule has no file: only compiling can fail, before anything
/// changed.
#[tokio::test]
async fn a_refused_temporary_rule_is_rejected() {
    let mut timed = export_rule(&bound("100-x", "deny"));
    timed["duration"] = json!("5m");
    let (outcome, _) = refused_result(Vec::new(), add(timed, Some("a")), "100-x").await;
    assert!(
        matches!(outcome, RuleCommandOutcome::Rejected { .. }),
        "{outcome:?}"
    );
}
