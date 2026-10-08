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
    let sent: Vec<Rule> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|n| n.rules[0].clone())
        .collect();
    let off = Rule {
        enabled: false,
        ..rule.clone()
    };
    assert_eq!(sent, [off, rule]);
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
