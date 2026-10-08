//! Toggles, expiry stamps and busy names (P2.1 re-review M2, M4, M5).

use super::tests::*;
use super::*;
use crate::test_daemon::*;
use serde_json::json;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Operator, Rule};
use std::time::Duration;

fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: r#type.into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn stock(name: &str, operator: Operator, duration: &str) -> Rule {
    Rule {
        name: name.into(),
        enabled: false,
        action: "allow".into(),
        duration: duration.into(),
        operator: Some(operator),
        ..Default::default()
    }
}

fn switched(rule: &Rule, on: bool) -> serde_json::Value {
    let mut wire = export_rule(rule);
    wire["enabled"] = json!(on);
    wire
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A disabled stock-UI rule that matches everything can't be turned on
/// from Snitchwatch; it can still be turned off, and an ordinary rule on.
#[tokio::test]
async fn turning_on_a_rule_that_matches_everything_is_refused() {
    let matching_all = [
        stock("100-true", leaf("simple", "true", ""), "always"),
        stock(
            "101-all",
            leaf("network", "dest.network", "0.0.0.0/0"),
            "always",
        ),
        stock("102-blank", leaf("simple", "dest.host", ""), "always"),
        stock(
            "103-forever",
            leaf("simple", "dest.host", "x.example"),
            "forever",
        ),
    ];
    let mut daemon = daemon(matching_all.to_vec());
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    for rule in &matching_all {
        commands.try_route(update(&rule.name, switched(rule, true), Some("t")));
        let reasons = refused(&result(&mut rx).await);
        assert!(
            reasons.iter().any(|r| r.contains("can't be turned on")),
            "{}: {reasons:?}",
            rule.name
        );
    }
    nothing_sent(&mut daemon).await;
}

#[tokio::test]
async fn turning_off_is_never_refused_and_an_ordinary_rule_turns_on() {
    let mut on = stock("100-true", leaf("simple", "true", ""), "always");
    on.enabled = true;
    let off = Rule {
        enabled: false,
        ..bound("200-ok", "deny")
    };
    let mut daemon = daemon(vec![on.clone(), off.clone()]);
    let seen = respond(&mut daemon, |_| Some((true, String::new()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-true", switched(&on, false), Some("a")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    commands.try_route(update("200-ok", switched(&off, true), Some("b")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(names(&seen), vec!["100-true", "200-ok"]);
}

/// The daemon starts a timed rule's clock only when the rule is on
/// (`loader.go` `replaceUserRule`): a disabled one gets no expiry stamp, and
/// turning it on starts the clock, unless one already runs from an earlier
/// time it was on.
#[tokio::test]
async fn only_an_enabled_timed_rule_gets_an_expiry_stamp() {
    let mut daemon = daemon(Vec::new());
    let seen = respond(&mut daemon, |_| Some((true, String::new()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut disabled = export_rule(&Rule {
        enabled: false,
        duration: "5m".into(),
        ..bound("100-x", "deny")
    });
    commands.try_route(add(disabled.clone(), Some("a")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(seen.lock().unwrap()[0].rules[0].created, 0);

    disabled["enabled"] = json!(true);
    commands.try_route(update("100-x", disabled.clone(), Some("b")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    let stamped = seen.lock().unwrap()[1].rules[0].created;
    assert!((now() - stamped).abs() < 5, "turned on: {stamped}");
}

/// After a resync every rule's `created` is the daemon's, which says
/// nothing about a clock: turning a timed rule on always stamps it now, so
/// the list can't hide an active rule early (a leftover row is the safer
/// mistake).
#[tokio::test]
async fn turning_a_timed_rule_on_stamps_it_now_even_after_a_resync() {
    let stale = now() - 4 * 60;
    let rule = Rule {
        enabled: false,
        duration: "5m".into(),
        created: stale,
        ..bound("100-x", "deny")
    };
    let mut daemon = daemon(vec![rule.clone()]);
    let seen = respond(&mut daemon, |_| Some((true, String::new()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update("100-x", switched(&rule, true), Some("a")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    let sent = seen.lock().unwrap()[0].rules[0].created;
    assert!((now() - sent).abs() < 5, "stamped now: {sent}");
    let mut cache = daemon.cache.lock().unwrap();
    assert!(
        cache.prune_expired(now() + 2 * 60).is_empty(),
        "still listed two minutes on"
    );
}

/// An edit can take two steps (refused, then the old rule restored): its
/// rule takes no other command meanwhile.
#[tokio::test]
async fn an_edit_keeps_its_rule_busy_until_it_ends() {
    let rule = bound("100-x", "deny");
    let mut daemon = daemon(vec![rule.clone()]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut edited = export_rule(&rule);
    edited["action"] = json!("reject");
    commands.try_route(update("100-x", edited, Some("e1")));
    commands.try_route(update("100-x", switched(&rule, false), Some("t1")));
    let (id, outcome) = result_within(&mut rx, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(id, "t1");
    assert!(refused(&outcome).iter().any(|r| r == BUSY), "{outcome:?}");
    let first = daemon.rx.recv().await.unwrap();
    assert_eq!(first.rules[0].action, "reject");
}

/// An add's name stays busy until the daemon answers: a second add, or a
/// rename onto it, can't race it.
#[tokio::test]
async fn an_add_being_saved_keeps_its_name() {
    let mut daemon = daemon(vec![bound("300-other", "deny")]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(add(export_rule(&bound("100-x", "deny")), Some("a1")));
    commands.try_route(add(export_rule(&bound("100-x", "allow")), Some("a2")));
    let (id, outcome) = result_within(&mut rx, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(id, "a2");
    assert!(refused(&outcome).iter().any(|r| r == BUSY), "{outcome:?}");
    commands.try_route(update(
        "300-other",
        export_rule(&bound("100-x", "deny")),
        Some("r1"),
    ));
    let (id, outcome) = result_within(&mut rx, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(id, "r1");
    assert!(refused(&outcome).iter().any(|r| r == BUSY), "{outcome:?}");
    let first = daemon.rx.recv().await.unwrap();
    assert_eq!(first.rules[0].name, "100-x");
    assert!(daemon.rx.try_recv().is_err(), "only the first add was sent");
}
