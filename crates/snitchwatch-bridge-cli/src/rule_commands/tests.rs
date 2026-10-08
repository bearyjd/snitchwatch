//! Rule commands (P2.1) against a real `DaemonCommands`: the gates before
//! anything is sent, the results, and #48's behaviour for older GUIs.

use super::*;
use crate::test_daemon::*;
use serde_json::{json, Value};
use snitchwatch_bridge::daemon_commands::DaemonTransport;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Action, Operator, Rule};
use std::time::Duration;
use tokio::sync::broadcast;

pub(super) fn commands(daemon: &Daemon) -> RuleCommands {
    RuleCommands::with_timeout(
        daemon.commands.clone(),
        daemon.cache.clone(),
        daemon.broadcast.clone(),
        crate::busy::BusyNames::default(),
        Duration::from_millis(300),
    )
}

pub(super) fn bound(name: &str, action: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: true,
        action: action.into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "list".into(),
            operand: "list".into(),
            list: vec![
                Operator {
                    r#type: "simple".into(),
                    operand: "process.path".into(),
                    data: "/usr/bin/curl".into(),
                    sensitive: true,
                    ..Default::default()
                },
                Operator {
                    r#type: "simple".into(),
                    operand: "dest.host".into(),
                    data: "example.com".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub(super) fn add(rule: Value, id: Option<&str>) -> ClientMessage {
    ClientMessage::AddRule {
        rule,
        request_id: id.map(str::to_string),
        reply: None,
    }
}

pub(super) fn update(rule_id: &str, rule: Value, id: Option<&str>) -> ClientMessage {
    ClientMessage::UpdateRule {
        rule_id: rule_id.into(),
        rule,
        request_id: id.map(str::to_string),
        reply: None,
    }
}

fn delete(rule_id: &str, id: Option<&str>) -> ClientMessage {
    ClientMessage::DeleteRule {
        rule_id: rule_id.into(),
        request_id: id.map(str::to_string),
        reply: None,
    }
}

/// The next `RuleCommandResult`, or `None` within `wait`.
pub(super) async fn result_within(
    rx: &mut broadcast::Receiver<ServerMessage>,
    wait: Duration,
) -> Option<(String, RuleCommandOutcome)> {
    tokio::time::timeout(wait, async {
        loop {
            if let Ok(ServerMessage::RuleCommandResult {
                request_id,
                outcome,
            }) = rx.recv().await
            {
                return (request_id, outcome);
            }
        }
    })
    .await
    .ok()
}

pub(super) async fn result(rx: &mut broadcast::Receiver<ServerMessage>) -> RuleCommandOutcome {
    result_within(rx, Duration::from_secs(5))
        .await
        .expect("no RuleCommandResult")
        .1
}

pub(super) fn refused(outcome: &RuleCommandOutcome) -> Vec<String> {
    match outcome {
        RuleCommandOutcome::Refused { problems } => {
            problems.iter().map(|p| p.reason.clone()).collect()
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Nothing reached the daemon's stream.
pub(super) async fn nothing_sent(daemon: &mut Daemon) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        daemon.rx.try_recv().is_err(),
        "a command reached the daemon"
    );
}

fn ok_all(daemon: &mut Daemon) -> Seen {
    respond(daemon, |_| Some((true, String::new()))).0
}

// --- Adding ---------------------------------------------------------------

#[tokio::test]
async fn an_added_rule_is_checked_by_the_editor_profile() {
    let mut daemon = daemon(Vec::new());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut timed = export_rule(&bound("100-x", "deny"));
    timed["duration"] = json!("1.5h");
    assert!(commands.try_route(add(timed, Some("e1"))).is_none());
    let problems = refused(&result(&mut rx).await);
    assert!(
        problems.iter().any(|p| p.contains("30s, 5m or 1h30m")),
        "{problems:?}"
    );
    nothing_sent(&mut daemon).await;
}

/// One add per `validate_operator` refusal: each is refused, none is sent.
#[tokio::test]
async fn every_pairing_refusal_reaches_the_editor_path() {
    let mut daemon = daemon(Vec::new());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let leaf = |t: &str, o: &str, d: &str| json!({ "type": t, "operand": o, "data": d });
    let path = leaf("simple", "process.path", "/usr/bin/curl");
    let many: Vec<Value> = (0..65).map(|_| path.clone()).collect();
    for operator in [
        leaf("network", "dest.ip", "10.0.0.0/8"),
        leaf("simple", "dest.network", "10.0.0.0/8"),
        leaf("simple", "list", "x"),
        json!({ "type": "list", "operands": [] }),
        json!({ "type": "list", "operands": many }),
        json!({ "type": "list", "operands": [{ "type": "list", "operands": [path] }] }),
        leaf("regexp", "true", "x"),
        leaf("lists", "lists.domains", "/etc"),
    ] {
        let rule = json!({ "name": "100-x", "enabled": true, "action": "deny",
                           "duration": "always", "operator": operator });
        commands.try_route(add(rule, Some("e")));
        refused(&result(&mut rx).await);
    }
    nothing_sent(&mut daemon).await;
}

#[tokio::test]
async fn an_add_never_overwrites_an_existing_or_hidden_rule() {
    let mut daemon = daemon(vec![bound("100-x", "deny")]);
    daemon
        .cache
        .lock()
        .unwrap()
        .set_left_out([("200-hidden".to_string(), 20_000)].into());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    for name in ["100-x", "200-hidden"] {
        commands.try_route(add(export_rule(&bound(name, "allow")), Some("e")));
        assert!(refused(&result(&mut rx).await)
            .iter()
            .any(|p| p == NAME_TAKEN));
    }
    nothing_sent(&mut daemon).await;
}

#[tokio::test]
async fn an_added_timed_rule_expires_in_the_cache_from_when_it_was_saved() {
    let mut daemon = daemon(Vec::new());
    let seen = ok_all(&mut daemon);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut timed = export_rule(&bound("100-x", "deny"));
    timed["duration"] = json!("5m");
    commands.try_route(add(timed, Some("e1")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent.r#type, Action::ChangeRule as i32);
    assert_eq!(sent.rules.len(), 1);
    let created = daemon.cache.lock().unwrap().rules().unwrap()["100-x"].created;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now - created).abs() < 5, "created {created}, now {now}");
}

// --- Toggles, edits and read-only rows -------------------------------------

#[tokio::test]
async fn a_pure_toggle_of_a_stock_rule_is_sent_as_before() {
    // A stock-UI rule the editor profile would refuse (a hash condition).
    let mut stock = bound("100-stock", "deny");
    stock.operator.as_mut().unwrap().list.push(Operator {
        r#type: "simple".into(),
        operand: "process.hash.md5".into(),
        data: "d41d8cd98f00b204e9800998ecf8427e".into(),
        ..Default::default()
    });
    let mut daemon = daemon(vec![stock.clone()]);
    let seen = ok_all(&mut daemon);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut toggled = export_rule(&stock);
    toggled["enabled"] = json!(false);
    // As the GUI sends a list back: no operand.
    toggled["operator"]
        .as_object_mut()
        .unwrap()
        .remove("operand");
    commands.try_route(update("100-stock", toggled, Some("t1")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen), vec!["100-stock"]);
    assert!(!daemon.cache.lock().unwrap().rules().unwrap()["100-stock"].enabled);
}

#[tokio::test]
async fn any_other_change_is_checked_by_the_editor_profile() {
    let cached = bound("100-x", "deny");
    let mut daemon = daemon(vec![cached.clone()]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut longer = export_rule(&cached);
    longer["duration"] = json!("1.5h");
    commands.try_route(update("100-x", longer, Some("u1")));
    refused(&result(&mut rx).await);
    let mut unbound = export_rule(&cached);
    unbound["action"] = json!("allow");
    unbound["operator"]["operands"][0]["data"] = json!("curl");
    commands.try_route(update("100-x", unbound, Some("u2")));
    assert!(refused(&result(&mut rx).await)
        .iter()
        .any(|p| p.contains("full path")));
    nothing_sent(&mut daemon).await;
}

#[tokio::test]
async fn read_only_and_hidden_rules_cannot_be_changed() {
    let uid_row = Rule {
        name: "100-uid".into(),
        operator: Some(Operator {
            r#type: "list".into(),
            operand: "list".into(),
            list: vec![
                bound("x", "deny").operator.unwrap().list[0].clone(),
                Operator {
                    r#type: "simple".into(),
                    operand: "user.name".into(),
                    data: "1000".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        ..bound("100-uid", "deny")
    };
    let mut daemon = daemon(vec![
        bound("z00-blocklist:ads:domains", "deny"),
        bound("000-snitchwatch-bridge-fetch", "allow"),
        uid_row,
    ]);
    daemon
        .cache
        .lock()
        .unwrap()
        .set_left_out([("200-hidden".to_string(), 20_000)].into());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    for name in [
        "z00-blocklist:ads:domains",
        "000-snitchwatch-bridge-fetch",
        "100-uid",
        "200-hidden",
        "300-unknown",
    ] {
        let mut toggled = export_rule(&bound(name, "deny"));
        toggled["enabled"] = json!(false);
        commands.try_route(update(name, toggled, Some("r")));
        let reasons = refused(&result(&mut rx).await);
        if name == "200-hidden" {
            // Said as it is: the rule exists but can't be shown.
            assert!(
                reasons.iter().any(|r| r.contains("too large")),
                "{reasons:?}"
            );
        }
    }
    nothing_sent(&mut daemon).await;
}

// --- Results --------------------------------------------------------------

#[tokio::test]
async fn each_outcome_reaches_the_asking_request() {
    let mut daemon = daemon(vec![bound("100-a", "deny"), bound("100-b", "deny")]);
    respond(&mut daemon, |name| match name {
        "100-a" => Some((false, "bad <b>regexp</b>\u{202e}".to_string())),
        "100-b" => None,
        _ => Some((true, String::new())),
    });
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let toggle = |name: &str| {
        let mut rule = export_rule(&bound(name, "deny"));
        rule["enabled"] = json!(false);
        rule
    };
    commands.try_route(update("100-a", toggle("100-a"), Some("ra")));
    let (id, outcome) = result_within(&mut rx, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(id, "ra");
    assert_eq!(
        outcome,
        RuleCommandOutcome::Rejected {
            reason: "bad <b>regexp</b>".into()
        }
    );
    commands.try_route(update("100-b", toggle("100-b"), Some("rb")));
    assert_eq!(
        result_within(&mut rx, Duration::from_secs(5))
            .await
            .unwrap(),
        ("rb".to_string(), RuleCommandOutcome::Timeout)
    );
    drop(daemon.registration.take());
    commands.try_route(delete("100-a", Some("rc")));
    assert_eq!(
        result_within(&mut rx, Duration::from_secs(5))
            .await
            .unwrap(),
        ("rc".to_string(), RuleCommandOutcome::NoDaemon)
    );
}

/// An older GUI's toggle carries no request id: no result, and a failure
/// re-sends the list to undo its optimistic switch (#48).
#[tokio::test]
async fn without_a_usable_request_id_there_is_no_result() {
    let mut daemon = daemon(vec![bound("100-a", "deny")]);
    respond(&mut daemon, |_| Some((false, "no".to_string())));
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut toggled = export_rule(&bound("100-a", "deny"));
    toggled["enabled"] = json!(false);
    for id in [None, Some("a/b"), Some(&*"a".repeat(65))] {
        commands.try_route(update("100-a", toggled.clone(), id));
        let set_rules = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await.unwrap() {
                    ServerMessage::SetRules { .. } => return,
                    ServerMessage::RuleCommandResult { .. } => panic!("a result for {id:?}"),
                    _ => {}
                }
            }
        })
        .await;
        assert!(set_rules.is_ok(), "the list was not re-sent for {id:?}");
    }
}

// --- The legacy transport ----------------------------------------------------

/// Review M5 applies here too: on TCP a GUI can toggle and delete as
/// before, but not add, edit or rename.
#[tokio::test]
async fn on_tcp_only_toggles_and_deletes_go_through() {
    let mut daemon = daemon_on(DaemonTransport::Tcp, vec![bound("100-a", "deny")]);
    let seen = ok_all(&mut daemon);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(add(export_rule(&bound("100-new", "deny")), Some("a")));
    assert!(refused(&result(&mut rx).await)
        .iter()
        .any(|p| p == TCP_REFUSED));
    let mut edited = export_rule(&bound("100-a", "deny"));
    edited["description"] = json!("note");
    commands.try_route(update("100-a", edited, Some("e")));
    assert!(refused(&result(&mut rx).await)
        .iter()
        .any(|p| p == TCP_REFUSED));
    commands.try_route(update(
        "100-a",
        export_rule(&bound("100-b", "deny")),
        Some("r"),
    ));
    assert!(refused(&result(&mut rx).await)
        .iter()
        .any(|p| p == TCP_REFUSED));

    let mut toggled = export_rule(&bound("100-a", "deny"));
    toggled["enabled"] = json!(false);
    commands.try_route(update("100-a", toggled, Some("t")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    commands.try_route(delete("100-a", Some("d")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen), vec!["100-a", "100-a"]);
}

#[tokio::test]
async fn other_messages_pass_through() {
    let daemon = daemon(Vec::new());
    assert_eq!(
        commands(&daemon).try_route(ClientMessage::Undo),
        Some(ClientMessage::Undo)
    );
}
