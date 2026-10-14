//! Prompt-slot D, plan item 13: a recommended background-service rule can be
//! turned on and off from the Rules page under the reserved-prefix check,
//! and nothing else.

use super::tests::*;
use super::*;
use crate::test_daemon::*;
use serde_json::json;
use snitchwatch_bridge::curated::entries;
use snitchwatch_bridge::rule_io::export_rule;
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::Rule;

fn shipped() -> Rule {
    let entry = entries()
        .iter()
        .find(|e| e.id == "flatpak-flathub")
        .unwrap();
    Rule {
        created: 1_700_000_000,
        ..entry.rule()
    }
}

fn switched(rule: &Rule, on: bool) -> serde_json::Value {
    let mut wire = export_rule(rule);
    wire["enabled"] = json!(on);
    wire
}

#[tokio::test]
async fn a_shipped_curated_rule_turns_off_and_on_again() {
    let rule = shipped();
    let mut daemon = daemon(vec![rule.clone()]);
    let seen = respond(&mut daemon, |_| Some((true, String::new()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, false), Some("off")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    commands.try_route(update(&rule.name, switched(&rule, true), Some("on")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    // `created` aside: the daemon stamps every change itself and ignores
    // the one sent, and a confirmed change restamps the cached rule (N5).
    let unstamped = |rule: &Rule| Rule {
        created: 0,
        ..rule.clone()
    };
    let sent: Vec<Rule> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| unstamped(&n.rules[0]))
        .collect();
    let off = Rule {
        enabled: false,
        ..unstamped(&rule)
    };
    assert_eq!(sent, [off, unstamped(&rule)]);
}

/// Everything but a pure toggle of a shipped entry's own rule is refused,
/// and nothing reaches the daemon.
#[tokio::test]
async fn nothing_but_a_toggle_of_a_shipped_rule_gets_through() {
    let rule = shipped();
    let squatter = Rule {
        name: "snitchwatch-default-squatter".into(),
        ..rule.clone()
    };
    let mut daemon = daemon(vec![rule.clone(), squatter.clone()]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut wider = switched(&rule, false);
    wider["operator"]["operands"][1]["data"] = json!("example.org");
    let mut renamed = switched(&rule, false);
    renamed["name"] = json!("my-flatpak");
    let attempts = [
        update(&rule.name, wider, Some("wider")),
        update(&rule.name, renamed, Some("renamed")),
        update(&squatter.name, switched(&squatter, false), Some("squatter")),
        add(switched(&rule, true), Some("add")),
        ClientMessage::DeleteRule {
            rule_id: rule.name.clone(),
            request_id: Some("delete".into()),
            reply: None,
        },
    ];
    for attempt in attempts {
        commands.try_route(attempt);
        assert!(!refused(&result(&mut rx).await).is_empty(), "not refused");
    }
    // A squatter is refused by the gate, with the reserved-name reason.
    commands.try_route(update(
        &squatter.name,
        switched(&squatter, true),
        Some("squatter-on"),
    ));
    assert_eq!(
        refused(&result(&mut rx).await),
        [snitchwatch_bridge::rule_policy::CURATED_MANAGED_REASON]
    );
    nothing_sent(&mut daemon).await;
}

/// A profile's rule (#46 Part 2) can't be toggled, edited, renamed or
/// deleted from a GUI either; the curated toggle exception doesn't reach it.
#[tokio::test]
async fn a_profile_rule_is_never_changed_from_the_rules_page() {
    let profile = Rule {
        name: "850-profile:home:0000-r1".into(),
        description: r#"{"snitchwatch":{"source":"profile"}}"#.into(),
        ..shipped()
    };
    let mut daemon = daemon(vec![profile.clone()]);
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    let mut renamed = switched(&profile, true);
    renamed["name"] = json!("my-copy");
    let attempts = [
        update(&profile.name, switched(&profile, false), Some("toggle")),
        update(&profile.name, renamed, Some("rename")),
        ClientMessage::DeleteRule {
            rule_id: profile.name.clone(),
            request_id: Some("delete".into()),
            reply: None,
        },
        add(switched(&profile, true), Some("add")),
    ];
    for attempt in attempts {
        commands.try_route(attempt);
        assert!(!refused(&result(&mut rx).await).is_empty(), "not refused");
    }
    nothing_sent(&mut daemon).await;
}

/// A bridge whose Recommended page changes nothing (the per-user one, or
/// unreadable choices) refuses a recommended rule's toggle too.
#[tokio::test]
async fn an_inert_bridge_refuses_toggling_a_recommended_rule() {
    let rule = shipped();
    let mut daemon = daemon(vec![rule.clone()]);
    let curated = CuratedDefaults::new(
        daemon.commands.clone(),
        daemon.cache.clone(),
        daemon.broadcast.clone(),
    );
    curated.set_unavailable("per-user");
    let commands = commands(&daemon).with_curated(curated);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, false), Some("off")));
    assert_eq!(refused(&result(&mut rx).await), [CURATED_INERT_REFUSED]);
    nothing_sent(&mut daemon).await;
}

#[tokio::test]
async fn generic_dns_reenable_refuses_but_off_and_narrow_toggles_remain_available() {
    let entry = entries()
        .iter()
        .find(|entry| entry.id == "dns-resolved")
        .unwrap();
    let mut rule = entry.rule_as(Some(990));
    rule.enabled = false;
    let mut daemon = daemon(vec![rule.clone()]);
    let seen = respond(&mut daemon, |_| Some((true, String::new()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, true), Some("dns-on")));
    assert!(matches!(
        result(&mut rx).await,
        RuleCommandOutcome::Refused { .. }
    ));
    assert!(seen.lock().unwrap().is_empty());
    commands.try_route(update(&rule.name, switched(&rule, false), Some("dns-off")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Ok);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// #120 item 12: only a turn-on the daemon answered `ERROR` is undone. One
/// it never answered may still be applied later, and an undo then could
/// race it: nothing more is sent.
#[tokio::test]
async fn an_unanswered_turn_on_is_not_undone() {
    let rule = Rule {
        enabled: false,
        ..shipped()
    };
    let mut daemon = daemon(vec![rule.clone()]);
    let seen = respond(&mut daemon, |_| None).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, true), Some("on")));
    assert_eq!(result(&mut rx).await, RuleCommandOutcome::Timeout);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "an undo was sent");
}

/// #120 item 12: a refused turn-on is undone before it is answered, and the
/// rule stays busy until then: a second turn-on in that window is refused,
/// so a late undo can never turn off a rule the user turned on again.
#[tokio::test]
async fn a_refused_turn_on_is_undone_before_another_toggle_is_taken() {
    let rule = Rule {
        enabled: false,
        ..shipped()
    };
    let mut daemon = daemon(vec![rule.clone()]);
    // The turn-on is refused; the undo gets no answer (300 ms timeout).
    let seen = respond_to(&mut daemon, |n| {
        n.rules[0]
            .enabled
            .then(|| (false, "save failed".to_string()))
    })
    .0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, true), Some("a")));
    for _ in 0..100 {
        if seen.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let sent: Vec<bool> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].enabled)
        .collect();
    assert_eq!(sent, [true, false]);
    commands.try_route(update(&rule.name, switched(&rule, true), Some("b")));
    let mut outcomes = std::collections::BTreeMap::new();
    while let Some((id, outcome)) = result_within(&mut rx, Duration::from_secs(2)).await {
        outcomes.insert(id, outcome);
        if outcomes.len() == 2 {
            break;
        }
    }
    assert!(
        matches!(outcomes.get("a"), Some(RuleCommandOutcome::Rejected { .. })),
        "{outcomes:?}"
    );
    assert_eq!(refused(&outcomes["b"]), [gates::BUSY]);
    assert_eq!(seen.lock().unwrap().len(), 2, "the second turn-on was sent");
}

/// #154: a refused turn-off of a recommended rule took it off in memory
/// (a disabled rule isn't compiled, so only `Save` failed): like any
/// `always` rule's (#120 item 3), the result says its file may turn it back
/// on at the next restart, and the list shows it off.
#[tokio::test]
async fn a_refused_turn_off_says_the_rule_may_come_back_on() {
    let rule = shipped();
    assert!(rule.enabled && rule.duration == "always");
    let mut daemon = daemon(vec![rule.clone()]);
    let seen = respond(&mut daemon, |_| Some((false, "save failed".into()))).0;
    let commands = commands(&daemon);
    let mut rx = daemon.broadcast.subscribe();
    commands.try_route(update(&rule.name, switched(&rule, false), Some("off")));
    match result(&mut rx).await {
        RuleCommandOutcome::OkWithNote { note } => {
            assert!(note.contains("may come back on"), "{note}");
            assert!(note.contains("save failed"), "{note}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!daemon.cache.lock().unwrap().rules().unwrap()[&rule.name].enabled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "a turn-off is never undone");
}
