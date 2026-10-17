use super::*;

#[test]
fn any_host_scope_matches_on_process_path() {
    let rule = once_rule(
        Verdict::Allow,
        VerdictScope::AnyHost,
        &sample_connection(),
        0,
    );
    let op = rule.operator.unwrap();
    assert_eq!(op.r#type, "simple");
    assert_eq!(op.operand, "process.path");
    assert_eq!(op.data, "/usr/bin/curl");
    // A non-sensitive simple operator compares with Unicode case folding
    // (`strings.EqualFold`, operator.go:224-226); Linux paths aren't.
    assert!(op.sensitive, "process.path must match case-sensitively");
}

#[test]
fn any_host_scope_names_the_rule_after_its_program() {
    let rule = verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::AnyHost,
        &sample_connection(),
        0,
    )
    .expect("absolute process path");
    assert!(rule.name.contains("-pcurl-"), "got: {}", rule.name);
}

/// Issue #44: with no process path, an `AnyHost` answer is never
/// remembered, and its once-only reply is never an operator matching
/// every process (which an empty `process.path` would amount to).
#[test]
fn any_host_scope_is_once_only_and_host_bound_when_process_path_is_empty() {
    let mut conn = sample_connection();
    conn.process_path = String::new();
    assert_eq!(
        verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Always,
            VerdictScope::AnyHost,
            &conn,
            0
        ),
        Err(RuleRefusal::ProcessFileUnknown)
    );
    let op = once_rule(Verdict::Allow, VerdictScope::AnyHost, &conn, 0)
        .operator
        .unwrap();
    assert_eq!(op.operand, "dest.host");
    assert_eq!(op.data, "github.com");
}

// -- issue #44: host/domain scopes are bound to the requesting process --

#[test]
fn this_host_scope_binds_a_remembered_rule_to_the_requesting_process() {
    let rule = verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
        &sample_connection(),
        0,
    )
    .expect("absolute process path");
    let host = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(host.r#type, "simple");
    assert_eq!(host.operand, "dest.host");
    assert_eq!(host.data, "github.com");
}

#[test]
fn this_host_scope_binds_a_bare_ip_rule_to_the_requesting_process() {
    let mut conn = sample_connection();
    conn.dst_host = String::new();
    let rule = verdict_to_rule(
        Verdict::Deny,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
        &conn,
        0,
    )
    .expect("absolute process path");
    let host = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(host.operand, "dest.ip");
    assert_eq!(host.data, "140.82.121.4");
}

#[test]
fn any_host_on_domain_scope_binds_the_wildcard_to_the_requesting_process() {
    let mut conn = sample_connection();
    conn.dst_host = "www.example.com".to_string();
    let rule = verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::AnyHostOnDomain,
        &conn,
        0,
    )
    .expect("absolute process path");
    let host = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(host.r#type, "regexp");
    assert_eq!(host.operand, "dest.host");
    assert_eq!(host.data, r"^(?:[^.]+\.)*example\.com$");
}

#[test]
fn degraded_domain_scope_is_still_bound_to_the_requesting_process() {
    // `example.com` is its own registrable domain, so the wildcard
    // degrades to the exact host — and must stay process-bound.
    let mut conn = sample_connection();
    conn.dst_host = "example.com".to_string();
    let (_, degradation) = build_operator_checked(VerdictScope::AnyHostOnDomain, &conn);
    assert_eq!(
        degradation,
        Some(ScopeDegradation::AtOrAboveRegistrableDomain)
    );
    let rule = verdict_to_rule(
        Verdict::Deny,
        VerdictDuration::Always,
        VerdictScope::AnyHostOnDomain,
        &conn,
        0,
    )
    .expect("absolute process path");
    let host = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(host.operand, "dest.host");
    assert_eq!(host.data, "example.com");
}

/// Issue #44, second half: the host-only fallback a host scope used to
/// remember for an unknown program now only answers this connection.
#[test]
fn host_scopes_refuse_to_remember_a_rule_without_a_process_path() {
    let mut conn = sample_connection();
    conn.process_path = String::new();
    for scope in [VerdictScope::ThisHost, VerdictScope::AnyHostOnDomain] {
        assert_eq!(
            verdict_to_rule(Verdict::Allow, VerdictDuration::Always, scope, &conn, 0),
            Err(RuleRefusal::ProcessFileUnknown),
            "scope {scope:?}"
        );
        let op = once_rule(Verdict::Allow, scope, &conn, 0).operator.unwrap();
        assert_eq!(op.operand, "dest.host", "scope {scope:?}: {op:?}");
        assert!(op.list.is_empty(), "scope {scope:?}: {op:?}");
    }
}

// -- issue #44: process-bound rules need process-distinct names. If two
// programs' rules for the same verdict/host/port shared a name, the
// daemon would store the second as `<name>-2` (`setUniqueName`,
// loader.go:332-341) while the bridge and the Rules page still call it
// `<name>` — so a later toggle or delete there would edit the *other*
// program's rule. --

#[test]
fn rule_name_for_distinguishes_programs_for_the_same_host_and_port() {
    let curl = rule_name_for(Verdict::Allow, "github.com", 443, "/usr/bin/curl");
    let firefox = rule_name_for(
        Verdict::Allow,
        "github.com",
        443,
        "/usr/lib64/firefox/firefox",
    );
    assert_ne!(curl, firefox);
}

#[test]
fn rule_name_for_distinguishes_programs_sharing_a_basename() {
    let system = rule_name_for(Verdict::Allow, "github.com", 443, "/usr/bin/curl");
    let dropped = rule_name_for(Verdict::Allow, "github.com", 443, "/tmp/curl");
    assert_ne!(system, dropped);
}

#[test]
fn rule_name_for_without_a_process_keeps_the_host_only_name() {
    // Pinned literally (digest = first 8 bytes of SHA-256("github.com"),
    // computed outside this crate) so the shared-sanitizer refactor
    // can't silently rename existing rules.
    assert_eq!(
        rule_name_for(Verdict::Deny, "github.com", 443, ""),
        "snitchwatch-deny-github.com-3aeb002460381c6f-443"
    );
}

#[test]
fn rule_name_for_neutralizes_the_process_path_but_keeps_its_basename() {
    // Canonical (a `..` path is never bound, so it gets no program part).
    let name = rule_name_for(Verdict::Allow, "github.com", 443, "/opt/evil dir/a b");
    assert!(!name.contains('/'), "no path separator may survive: {name}");
    assert!(!name.contains(' '), "{name}");
    assert!(
        name.contains("-pa_b-"),
        "readable basename expected: {name}"
    );
}

#[test]
fn verdict_to_rule_names_a_process_bound_rule_after_its_program() {
    let conn = sample_connection();
    let rule = verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
        &conn,
        0,
    )
    .expect("absolute process path");
    assert_eq!(
        rule.name,
        rule_name_for(Verdict::Allow, "github.com", 443, "/usr/bin/curl")
    );
    assert!(rule.name.contains("-pcurl-"), "got: {}", rule.name);
}
