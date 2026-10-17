use super::*;

fn sample_connection() -> Connection {
    Connection {
        protocol: "tcp".to_string(),
        dst_ip: "140.82.121.4".to_string(),
        dst_host: "github.com".to_string(),
        dst_port: 443,
        process_path: "/usr/bin/curl".to_string(),
        ..Default::default()
    }
}

#[test]
fn allow_verdict_produces_allow_rule_with_once_duration() {
    let rule = once_rule(
        Verdict::Allow,
        VerdictScope::ThisHost,
        &sample_connection(),
        1_700_000_000,
    );
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(rule.enabled);
    assert_eq!(rule.created, 1_700_000_000);
    assert!(rule.name.contains("allow"));
}

#[test]
fn deny_verdict_produces_deny_rule() {
    let rule = once_rule(
        Verdict::Deny,
        VerdictScope::ThisHost,
        &sample_connection(),
        1_700_000_000,
    );
    assert_eq!(rule.action, "deny");
    assert_eq!(rule.duration, "once");
    assert!(rule.name.contains("deny"));
}

#[test]
fn rule_name_includes_remote_host_for_traceability() {
    let rule = once_rule(
        Verdict::Allow,
        VerdictScope::ThisHost,
        &sample_connection(),
        0,
    );
    assert!(rule.name.contains("github.com"), "got: {}", rule.name);
}

#[test]
fn rule_name_for_matches_verdict_to_rule_name() {
    // Single source of truth: the name the daemon gets back (via
    // `verdict_to_rule`) and the name the UI shows as `matched_rule` (via
    // `rule_name_for`, called from `ConnectionCache::resolve`) must be
    // identical for the same inputs, or the "Show rule" jump would land
    // on a rule that doesn't exist.
    let conn = sample_connection();
    let rule = once_rule(Verdict::Allow, VerdictScope::ThisHost, &conn, 1_700_000_000);
    let name = rule_name_for(
        Verdict::Allow,
        &conn.dst_host,
        conn.dst_port as u16,
        &conn.process_path,
    );
    assert_eq!(rule.name, name);
}

#[test]
fn each_ui_duration_maps_to_the_expected_daemon_string() {
    let conn = sample_connection();
    let cases = [
        (VerdictDuration::Once, "once"),
        (VerdictDuration::FiveMinutes, "5m"),
        (VerdictDuration::UntilRestart, "until restart"),
        (VerdictDuration::Always, "always"),
    ];
    for (duration, expected) in cases {
        let rule = verdict_to_rule(Verdict::Allow, duration, VerdictScope::ThisHost, &conn, 0)
            .expect("absolute process path");
        assert_eq!(rule.duration, expected, "duration mapping for {duration:?}");
    }
}

// -- operator population (issue #14) --------------------------------

#[test]
fn every_rule_carries_a_non_none_operator() {
    // The daemon hard-rejects `operator: None` (rule.go's Deserialize),
    // so this must never regress for any scope.
    for scope in [
        VerdictScope::ThisHost,
        VerdictScope::AnyHostOnDomain,
        VerdictScope::AnyHost,
    ] {
        let rule = once_rule(Verdict::Allow, scope, &sample_connection(), 0);
        assert!(
            rule.operator.is_some(),
            "scope {scope:?} produced a None operator"
        );
    }
}

/// The `[process.path, <host op>]` pair a process-bound `ThisHost` /
/// `AnyHostOnDomain` rule must carry. Panics unless the rule is exactly
/// that shape for `process_path`, then returns the host member.
fn process_bound_host_member(rule: Rule, process_path: &str) -> Operator {
    let op = rule.operator.expect("every rule carries an operator");
    assert_eq!(op.r#type, "list", "scope must be process-bound: {op:?}");
    assert_eq!(op.operand, "list");
    assert_eq!(op.list.len(), 2, "exactly process + host: {op:?}");
    let process = &op.list[0];
    assert_eq!(process.r#type, "simple");
    assert_eq!(process.operand, "process.path");
    assert_eq!(process.data, process_path);
    assert!(
        process.sensitive,
        "process.path must match case-sensitively"
    );
    op.list[1].clone()
}

mod any_host_process;
mod degradation_names;
mod display;
mod domain_scope;
