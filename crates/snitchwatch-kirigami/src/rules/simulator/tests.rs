//! Table tests for `simulate`, written against opensnitchd v1.8.0's
//! behaviour (`vendor/opensnitch/daemon/rule/operator.go` `Match`/`Compile`
//! and `loader.go` `FindFirstMatch`/`sortRules`) read off the Go source, not
//! against what the Rust happened to do.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use snitchwatch_bridge::ws_messages::ServerMessage;

use super::*;

mod hashes;
mod operands;
mod regexp_corpus;
mod regexps;

// ---- builders -------------------------------------------------------------

fn op(kind: &str, operand: &str, data: &str) -> Value {
    json!({"type": kind, "operand": operand, "data": data, "sensitive": false, "list": []})
}

fn op_sensitive(kind: &str, operand: &str, data: &str) -> Value {
    json!({"type": kind, "operand": operand, "data": data, "sensitive": true, "list": []})
}

fn simple(operand: &str, data: &str) -> Value {
    op("simple", operand, data)
}

fn list_op(children: Vec<Value>) -> Value {
    json!({"type": "list", "operand": "list", "data": "", "sensitive": false, "list": children})
}

fn rule_with(name: &str, enabled: bool, action: &str, precedence: bool, operator: Value) -> Value {
    json!({
        "name": name,
        "enabled": enabled,
        "action": action,
        "duration": "always",
        "description": "",
        "precedence": precedence,
        "operator": operator
    })
}

fn allow(name: &str, operator: Value) -> Value {
    rule_with(name, true, "allow", false, operator)
}

fn deny(name: &str, operator: Value) -> Value {
    rule_with(name, true, "deny", false, operator)
}

fn store_with(rules: Vec<Value>) -> RulesStore {
    let mut s = RulesStore::new();
    s.apply(&ServerMessage::SetRules { rules });
    s
}

fn base() -> SimulationInput {
    SimulationInput {
        process_path: Some("/usr/bin/curl".to_string()),
        dest_host: "example.com".to_string(),
        dest_port: 443,
        protocol: "tcp".to_string(),
        ..Default::default()
    }
}

fn base_with(f: impl FnOnce(&mut SimulationInput)) -> SimulationInput {
    let mut input = base();
    f(&mut input);
    input
}

fn env_of(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn paths(items: &[&str]) -> Option<Vec<String>> {
    Some(items.iter().map(|s| s.to_string()).collect())
}

/// Run one allow rule named `r` holding `operator`.
fn run(operator: Value, input: &SimulationInput) -> SimulationResult {
    simulate(&store_with(vec![allow("r", operator)]), input)
}

fn matched(operator: Value, input: &SimulationInput) -> bool {
    run(operator, input).matched_rule.is_some()
}

/// Rule names the scan decided on, for ordering tests.
fn decided(rules: Vec<Value>, input: &SimulationInput) -> (Option<String>, Option<String>) {
    let r = simulate(&store_with(rules), input);
    (r.matched_rule, r.action)
}

// ---- carried over from the original simulator ------------------------------

#[test]
fn no_rules_yields_no_match() {
    let result = simulate(&RulesStore::new(), &base());
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.action, None);
    assert_eq!(result.precedence, None);
}

#[test]
fn disabled_rules_are_skipped() {
    let (rule, action) = decided(
        vec![
            rule_with(
                "000-disabled",
                false,
                "deny",
                false,
                simple("dest.host", "example.com"),
            ),
            allow("899-allow", simple("process.path", "/usr/bin/curl")),
        ],
        &base(),
    );
    assert_eq!(rule.as_deref(), Some("899-allow"));
    assert_eq!(action.as_deref(), Some("allow"));
}

#[test]
fn a_disabled_precedence_rule_does_not_stop_the_scan() {
    let (rule, _) = decided(
        vec![
            rule_with("100-off", false, "allow", true, simple("protocol", "tcp")),
            allow("200-a", simple("protocol", "tcp")),
            allow("300-b", simple("dest.host", "example.com")),
        ],
        &base(),
    );
    assert_eq!(rule.as_deref(), Some("300-b"));
}

#[test]
fn true_operand_always_matches() {
    assert!(matched(simple("true", ""), &base()));
}

#[test]
fn regexp_wildcard_style_host_match() {
    let pattern = r"^[^.]*\.tracker\.example$";
    let hit = base_with(|i| i.dest_host = "ads.tracker.example".to_string());
    assert!(matched(op("regexp", "dest.host", pattern), &hit));
    assert!(!matched(op("regexp", "dest.host", pattern), &base()));
}

#[test]
fn list_operator_requires_every_child_to_match() {
    let operator = list_op(vec![
        simple("process.path", "/usr/bin/curl"),
        simple("dest.host", "example.com"),
    ]);
    assert!(matched(operator.clone(), &base()));
    let other_host = base_with(|i| i.dest_host = "slack.com".to_string());
    assert!(!matched(operator, &other_host));
}

// ---- unknown inputs are unevaluated, never guessed --------------------------

#[test]
fn an_unknown_input_is_unevaluated_and_never_a_match() {
    // (operator, the missing input's wording)
    let cases: Vec<(Value, &str)> = vec![
        (simple("process.parent.path", "/bin/bash"), "parent process"),
        (simple("process.command", "curl"), "command line"),
        (simple("process.id", "1"), "process ID"),
        (simple("user.id", "1000"), "user ID"),
        (simple("process.env.HOME", ""), "environment"),
        (simple("source.ip", "10.0.0.1"), "source IP"),
        (simple("source.port", "1"), "source port"),
        (simple("dest.ip", "10.0.0.1"), "destination IP"),
        (
            op("network", "dest.network", "10.0.0.0/8"),
            "destination IP",
        ),
        (op("network", "source.network", "10.0.0.0/8"), "source IP"),
        (simple("iface.in", "eth0"), "inbound interface"),
        (simple("iface.out", "eth0"), "outbound interface"),
    ];
    for (operator, wording) in cases {
        let result = run(operator.clone(), &base());
        assert_eq!(result.matched_rule, None, "{operator} matched on a guess");
        assert_eq!(
            result.unevaluated.len(),
            1,
            "{operator}: {:?}",
            result.unevaluated
        );
        let u = &result.unevaluated[0];
        assert_eq!(u.rule, "r");
        assert!(u.missing.contains(wording), "{operator}: {}", u.missing);
    }
}

#[test]
fn an_empty_regexp_on_an_unknown_input_is_still_unevaluated() {
    // Even a pattern that would match "" must not be run against a guess.
    let result = run(op("regexp", "user.id", ".*"), &base());
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
}

#[test]
fn an_invalid_ip_is_reported_as_unevaluated_not_compared() {
    let input = base_with(|i| i.dest_ip = Some("not-an-ip".to_string()));
    let result = run(simple("dest.ip", "not-an-ip"), &input);
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    // Typed but not an address: its own wording, not "left blank".
    assert!(result.unevaluated[0].invalid);
    assert!(result.unevaluated[0].missing.contains("destination IP"));
    // A blank one is the other kind.
    let blank = run(simple("dest.ip", "10.0.0.1"), &base());
    assert!(!blank.unevaluated[0].invalid);
}

#[test]
fn a_failing_list_member_hides_unknowns_that_cannot_matter() {
    // AND short-circuits: the path already fails, so the unknown uid is not
    // reported, and the rule is simply a non-match.
    let operator = list_op(vec![
        simple("process.path", "/usr/bin/wget"),
        simple("user.id", "1000"),
    ]);
    let result = run(operator, &base());
    assert_eq!(result.matched_rule, None);
    assert!(result.unevaluated.is_empty(), "{:?}", result.unevaluated);

    // Same when the unknown comes first: a later definite false decides.
    let operator = list_op(vec![
        simple("user.id", "1000"),
        simple("process.path", "/usr/bin/wget"),
    ]);
    let result = run(operator, &base());
    assert!(result.unevaluated.is_empty(), "{:?}", result.unevaluated);

    // With the path matching, the uid is what stops the rule being decided.
    let operator = list_op(vec![
        simple("process.path", "/usr/bin/curl"),
        simple("user.id", "1000"),
    ]);
    let result = run(operator, &base());
    assert_eq!(result.matched_rule, None);
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].operand, "user.id");
}

#[test]
fn a_list_stops_at_the_first_failing_child_so_later_unsupported_operands_are_not_visited() {
    let operator = list_op(vec![
        simple("process.path", "/usr/bin/wget"),
        simple("user.name", "alice"),
    ]);
    let result = run(operator, &base());
    assert!(result.unsupported_operands.is_empty());
}

#[test]
fn an_undetermined_deny_is_reported_even_though_a_later_allow_decides() {
    let rules = vec![
        deny("100-deny-uid", simple("user.id", "1000")),
        allow("200-allow", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("200-allow"));
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "100-deny-uid");
}

#[test]
fn an_undetermined_allow_before_the_deciding_rule_cannot_change_it_and_is_not_reported() {
    let rules = vec![
        allow("100-allow-uid", simple("user.id", "1000")),
        allow("200-allow", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("200-allow"));
    assert!(result.unevaluated.is_empty(), "{:?}", result.unevaluated);

    // After it, an undetermined allow could have overwritten the verdict.
    let rules = vec![
        allow("100-allow", simple("dest.host", "example.com")),
        allow("200-allow-uid", simple("user.id", "1000")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("100-allow"));
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "200-allow-uid");
}

#[test]
fn rules_after_the_stop_rule_are_never_visited() {
    let rules = vec![
        deny("100-deny", simple("dest.host", "example.com")),
        allow("200-allow-uid", simple("user.id", "1000")),
        allow("300-lists", op("lists", "lists.domains", "/x")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("100-deny"));
    assert!(result.unevaluated.is_empty());
    assert!(result.unsupported_operands.is_empty());
}

// ---- rule order ------------------------------------------------------------

#[test]
fn a_precedence_allow_stops_the_scan_before_a_later_deny() {
    let rules = vec![
        rule_with(
            "100-allow-first",
            true,
            "allow",
            true,
            simple("protocol", "tcp"),
        ),
        deny("200-deny", simple("dest.host", "example.com")),
    ];
    let (rule, action) = decided(rules, &base());
    assert_eq!(rule.as_deref(), Some("100-allow-first"));
    assert_eq!(action.as_deref(), Some("allow"));
}

#[test]
fn a_non_precedence_allow_does_not_stop_the_scan() {
    let rules = vec![
        allow("100-allow", simple("protocol", "tcp")),
        deny("200-deny", simple("dest.host", "example.com")),
    ];
    let (rule, action) = decided(rules, &base());
    assert_eq!(rule.as_deref(), Some("200-deny"));
    assert_eq!(action.as_deref(), Some("deny"));
}

#[test]
fn deny_and_reject_both_stop_the_scan() {
    for action in ["deny", "reject"] {
        let rules = vec![
            rule_with("100-stop", true, action, false, simple("protocol", "tcp")),
            allow("200-allow", simple("dest.host", "example.com")),
        ];
        let (rule, shown) = decided(rules, &base());
        assert_eq!(rule.as_deref(), Some("100-stop"), "{action}");
        // Reject is shown as deny; it decides the same way.
        assert_eq!(shown.as_deref(), Some("deny"), "{action}");
    }
}

#[test]
fn the_last_matching_allow_wins_when_nothing_stops_the_scan() {
    let rules = vec![
        allow("100-allow-a", simple("protocol", "tcp")),
        allow("200-allow-b", simple("dest.host", "example.com")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("200-allow-b"));
}

#[test]
fn rules_are_evaluated_in_name_order_whatever_order_the_store_holds() {
    // `UpdateRules` appends, so the store is not necessarily name-sorted; the
    // daemon sorts enabled rules by name (`sortRules`: sort.Strings).
    let rules = vec![
        allow("b-allow", simple("protocol", "tcp")),
        allow("a-allow", simple("protocol", "tcp")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("b-allow"));

    let rules = vec![
        deny("z-deny", simple("protocol", "tcp")),
        rule_with(
            "a-precedence-allow",
            true,
            "allow",
            true,
            simple("protocol", "tcp"),
        ),
    ];
    let (rule, action) = decided(rules, &base());
    assert_eq!(rule.as_deref(), Some("a-precedence-allow"));
    assert_eq!(action.as_deref(), Some("allow"));
}

#[test]
fn name_order_is_bytewise_like_go_sort_strings() {
    // 'B' (0x42) sorts before 'a' (0x61); a case-insensitive sort would not.
    let rules = vec![
        allow("a-allow", simple("protocol", "tcp")),
        allow("B-allow", simple("protocol", "tcp")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("a-allow"));
}

#[test]
fn the_reported_position_is_the_rules_row_in_the_list() {
    let rules = vec![
        rule_with("000-off", false, "allow", false, simple("true", "")),
        allow("100-on", simple("protocol", "tcp")),
    ];
    let result = simulate(&store_with(rules), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("100-on"));
    assert_eq!(result.precedence, Some(1));
}

// ---- results are plain data the UI can show --------------------------------

#[test]
fn the_result_serializes_with_camel_case_keys_the_page_reads() {
    let result = run(simple("user.name", "alice"), &base());
    let json = serde_json::to_value(&result).unwrap();
    for key in [
        "matchedRule",
        "action",
        "precedence",
        "unsupportedOperands",
        "unevaluated",
        "warnings",
    ] {
        assert!(json.get(key).is_some(), "missing {key}: {json}");
    }
    assert_eq!(json["unsupportedOperands"][0]["operand"], "user.name");
}

#[test]
fn rule_names_in_results_are_the_display_names() {
    // The bridge strips bidi overrides and zero-width characters from a
    // rule's display name; the result must not bring the raw name back.
    let mut named = allow("evil\u{202E}rule", simple("dest.host", "example.com"));
    named["displayName"] = json!("evilrule");
    let mut undecided = deny("evil\u{202E}uid", simple("user.id", "1000"));
    undecided["displayName"] = json!("eviluid");
    let result = simulate(&store_with(vec![named, undecided]), &base());
    assert_eq!(result.matched_rule.as_deref(), Some("evilrule"));
    assert_eq!(result.unevaluated.len(), 1);
    assert_eq!(result.unevaluated[0].rule, "eviluid");
}

#[test]
fn the_shown_action_is_compared_exactly_like_the_daemon_does() {
    // `acceptOrDeny`: `r.Action == rule.Allow` accepts; anything else drops.
    let shown = |action: &str| {
        let rule = rule_with("r", true, action, false, simple("true", ""));
        simulate(&store_with(vec![rule]), &base()).action
    };
    assert_eq!(shown("allow").as_deref(), Some("allow"));
    assert_eq!(shown("deny").as_deref(), Some("deny"));
    assert_eq!(shown("reject").as_deref(), Some("deny"));
    assert_eq!(shown("ALLOW").as_deref(), Some("deny"));
    assert_eq!(shown("").as_deref(), Some("deny"));
}

#[test]
fn an_action_spelled_differently_does_not_stop_the_scan() {
    // `FindFirstMatch` stops only on exactly "deny"/"reject".
    let rules = vec![
        rule_with("100-odd", true, "Deny", false, simple("true", "")),
        allow("200-allow", simple("true", "")),
    ];
    let (rule, action) = decided(rules, &base());
    assert_eq!(rule.as_deref(), Some("200-allow"));
    assert_eq!(action.as_deref(), Some("allow"));
}
