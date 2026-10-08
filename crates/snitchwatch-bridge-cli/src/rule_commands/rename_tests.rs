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
    let model = model(&[bound("100-old", "deny")]);
    let seen = run_model(&mut daemon, model.clone());
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
    assert_eq!(applying(&model), vec!["200-new"]);
    assert_eq!(files(&model), vec!["200-new"]);
}

#[tokio::test]
async fn a_refused_new_rule_leaves_the_old_one_alone() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let model = model(&[bound("100-old", "deny")]);
    let mut broken = renamed();
    broken["operator"]["operands"][1]["data"] = json!("broken.example");
    model
        .lock()
        .unwrap()
        .uncompilable
        .insert("broken.example".into());
    let seen = run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", broken, Some("r1")));
    assert!(matches!(
        result(&mut rx).await,
        RuleCommandOutcome::Rejected { .. }
    ));
    assert_eq!(steps(&seen), vec![("change", "200-new".to_string())]);
    assert_eq!(cached_names(&daemon), vec!["100-old"]);
    assert_eq!(applying(&model), vec!["100-old"]);
}

/// The daemon drops a rule from memory before removing its file, so an
/// ERROR on the old rule's delete means it already stopped applying. Undoing
/// the new rule then would leave neither; the rename stands, and the result
/// says the old file may bring the old rule back at a restart.
#[tokio::test]
async fn an_old_rule_whose_file_wont_go_is_renamed_anyway() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let model = model(&[bound("100-old", "deny")]);
    model.lock().unwrap().stuck_files.insert("100-old".into());
    let seen = run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    match result(&mut rx).await {
        RuleCommandOutcome::OkWithNote { note } => {
            assert!(note.contains("may come back"), "{note}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(steps(&seen).len(), 2, "no undo: {:?}", steps(&seen));
    assert_eq!(applying(&model), vec!["200-new"], "a rule still applies");
    assert_eq!(cached_names(&daemon), vec!["200-new"]);
}

/// An unanswered delete may have worked: removing the new rule then could
/// leave neither (a deny gone), so nothing is undone, and the result says
/// which rule decides while both may exist.
#[tokio::test]
async fn an_unanswered_delete_is_not_undone() {
    let mut daemon = daemon(vec![bound("100-old", "deny")]);
    let model = model(&[bound("100-old", "deny")]);
    model
        .lock()
        .unwrap()
        .silent
        .insert((Action::DeleteRule as i32, "100-old".into()));
    let seen = run_model(&mut daemon, model.clone());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-old", renamed(), Some("r1")));
    match result(&mut rx).await {
        RuleCommandOutcome::Unsure { reason } => {
            assert!(reason.contains("both may exist"), "{reason}");
            assert!(reason.contains("old rule decides"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(steps(&seen).len(), 2, "no undo: {:?}", steps(&seen));
}

/// `FindFirstMatch`: the first matching deny, reject or decide-first rule
/// by name wins; an allow only where nothing blocks.
#[test]
fn which_rule_decides_while_both_exist() {
    let rule = |name: &str, action: &str| bound(name, action);
    use super::rename::{deciding, BOTH_ALLOW, NEW_DECIDES, OLD_DECIDES};
    assert_eq!(
        deciding(&rule("100-a", "deny"), &rule("200-b", "allow")),
        OLD_DECIDES
    );
    assert_eq!(
        deciding(&rule("200-a", "allow"), &rule("100-b", "reject")),
        NEW_DECIDES
    );
    assert_eq!(
        deciding(&rule("200-a", "deny"), &rule("100-b", "deny")),
        NEW_DECIDES
    );
    assert_eq!(
        deciding(&rule("100-a", "allow"), &rule("200-b", "allow")),
        BOTH_ALLOW
    );
    let first = snitchwatch_proto::protocol::Rule {
        precedence: true,
        ..rule("300-a", "allow")
    };
    assert_eq!(deciding(&first, &rule("100-b", "allow")), OLD_DECIDES);
    let off = snitchwatch_proto::protocol::Rule {
        enabled: false,
        ..rule("100-a", "deny")
    };
    assert_eq!(deciding(&off, &rule("200-b", "allow")), NEW_DECIDES);
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

/// A rename changes the old rule, so the old rule must be one Snitchwatch
/// may change, and the renamed rule must pass the editor's checks.
#[tokio::test]
async fn a_rename_needs_a_changeable_old_rule_and_a_valid_new_one() {
    let mut locked = bound("100-locked", "deny");
    locked.operator.as_mut().unwrap().list[1] = snitchwatch_proto::protocol::Operator {
        r#type: "simple".into(),
        operand: "user.name".into(),
        data: "1000".into(),
        ..Default::default()
    };
    let mut daemon = daemon(vec![bound("100-old", "deny"), locked]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-locked", renamed(), Some("r1")));
    let reasons = refused(&result(&mut rx).await);
    assert!(
        reasons
            .iter()
            .any(|r| r == snitchwatch_bridge::rule_policy::SHAPE_READ_ONLY_REASON),
        "{reasons:?}"
    );
    let mut relative = renamed();
    relative["operator"]["operands"][0]["data"] = json!("curl");
    commands.try_route(update("100-old", relative, Some("r2")));
    let reasons = refused(&result(&mut rx).await);
    assert!(
        reasons.iter().any(|r| r.contains("full path")),
        "{reasons:?}"
    );
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
