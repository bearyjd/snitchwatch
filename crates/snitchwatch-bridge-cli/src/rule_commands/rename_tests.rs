//! Rename (P2.1, E1): `CHANGE_RULE` the new name, then, only on OK,
//! `DELETE_RULE` the old one. A failure never leaves neither rule; it leaves
//! both only when a second step fails too or goes unanswered, and says so.

use super::tests::*;
use super::*;
use crate::test_daemon::*;
use serde_json::json;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Action, Notification};
use std::time::Duration;

fn step(n: &Notification) -> (&'static str, String) {
    let action = if n.r#type == Action::ChangeRule as i32 {
        "change"
    } else if n.r#type == Action::DeleteRule as i32 {
        "delete"
    } else {
        "other"
    };
    (action, n.rules[0].name.clone())
}

fn steps(seen: &Seen) -> Vec<(&'static str, String)> {
    seen.lock().unwrap().iter().map(step).collect()
}

fn renamed() -> serde_json::Value {
    export_rule(&bound("200-new", "deny"))
}

/// The responder's answer for each (action, name).
fn answering(daemon: &mut Daemon, answer: fn(&str, &str) -> Option<bool>) -> Seen {
    respond_to(daemon, move |n| {
        let (action, name) = step(n);
        answer(action, &name).map(|ok| (ok, if ok { "" } else { "refused" }.to_string()))
    })
    .0
}

fn cached_names(daemon: &Daemon) -> Vec<String> {
    daemon
        .cache
        .lock()
        .unwrap()
        .rules()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_rename_adds_the_new_name_then_deletes_the_old_one() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let seen = answering(&mut daemon, |_, _| Some(true));
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(
        steps(&seen),
        vec![
            ("change", "200-new".to_string()),
            ("delete", "100-old".to_string())
        ]
    );
    assert_eq!(cached_names(&daemon), vec!["200-new"]);
}

#[tokio::test]
async fn a_refused_new_rule_leaves_the_old_one_alone() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let seen = answering(&mut daemon, |action, _| Some(action != "change"));
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    assert!(matches!(
        result(&mut rx).await,
        RuleCommandOutcome::Rejected { .. }
    ));
    assert_eq!(steps(&seen), vec![("change", "200-new".to_string())]);
    assert_eq!(cached_names(&daemon), vec!["100-old"]);
}

#[tokio::test]
async fn an_old_rule_that_wont_go_is_undone_by_removing_the_new_one() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let seen = answering(&mut daemon, |action, name| {
        Some(!(action == "delete" && name == "100-old"))
    });
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    match result(&mut rx).await {
        RuleCommandOutcome::Rejected { reason } => {
            assert!(reason.contains("Nothing changed"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        steps(&seen),
        vec![
            ("change", "200-new".to_string()),
            ("delete", "100-old".to_string()),
            ("delete", "200-new".to_string())
        ]
    );
    assert_eq!(cached_names(&daemon), vec!["100-old"]);
}

#[tokio::test]
async fn when_the_undo_fails_too_both_rules_exist_and_it_says_so() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    answering(&mut daemon, |action, _| Some(action == "change"));
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    match result(&mut rx).await {
        RuleCommandOutcome::Unsure { reason } => {
            assert!(reason.contains("both exist"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

/// An unanswered delete may have worked: removing the new rule then could
/// leave neither (a deny gone), so nothing is undone and it says so.
#[tokio::test]
async fn an_unanswered_delete_is_not_undone() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let seen = answering(&mut daemon, |action, _| {
        (action == "change").then_some(true)
    });
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    match result(&mut rx).await {
        RuleCommandOutcome::Unsure { reason } => {
            assert!(reason.contains("both may exist"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(steps(&seen).len(), 2, "no undo: {:?}", steps(&seen));
}

#[tokio::test]
async fn a_rename_onto_an_existing_hidden_or_reserved_name_is_refused() {
    let mut daemon = daemon(vec![bound("100-old", "deny"), bound("200-taken", "allow")]);
    daemon
        .cache
        .lock()
        .unwrap()
        .set_left_out([("300-hidden".to_string(), 20_000)].into());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    for target in ["200-taken", "300-hidden"] {
        commands.try_route(update(
            "100-old",
            export_rule(&bound(target, "deny")),
            Some("r"),
        ));
        assert!(refused(&result(&mut rx).await)
            .iter()
            .any(|p| p == NAME_TAKEN));
    }
    commands.try_route(update(
        "100-old",
        export_rule(&bound("z00-blocklist:x:domains", "deny")),
        Some("r"),
    ));
    refused(&result(&mut rx).await);
    nothing_sent(&mut daemon).await;
}

/// A deny renamed into an allow is still a change of that deny: the rename
/// is checked against the old rule, not as a new one.
#[tokio::test]
async fn a_rename_is_checked_against_the_rule_it_replaces() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut loosened = export_rule(&bound("200-new", "allow"));
    loosened["operator"]["operands"][0]["data"] = json!("curl");
    commands.try_route(update("100-old", loosened, Some("r")));
    refused(&result(&mut rx).await);
    nothing_sent(&mut daemon).await;
}

/// While a rename runs, its two names take no other command.
#[tokio::test]
async fn names_being_renamed_take_no_other_command() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    // Nobody answers: the rename waits on its first step.
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    let mut toggled = export_rule(&bound("100-old", "deny"));
    toggled["enabled"] = json!(false);
    commands.try_route(update("100-old", toggled, Some("r2")));
    let (id, outcome) = result_within(&mut rx, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(id, "r2");
    assert!(refused(&outcome).iter().any(|p| p == BUSY));
    let first = daemon.rx.recv().await.unwrap();
    assert_eq!(step(&first), ("change", "200-new".to_string()));
}
