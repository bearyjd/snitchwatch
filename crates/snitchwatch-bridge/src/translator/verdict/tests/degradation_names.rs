use super::*;

// -- FIX 2: Deny must never silently narrow ---------------------------

#[test]
fn allow_verdict_degradation_is_silent() {
    let mut conn = sample_connection();
    conn.dst_host = "shop.co.uk".to_string(); // would degrade
    let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Allow, &conn);
    assert!(
        reason.is_none(),
        "Allow degradation must not be surfaced (fail-safe narrowing)"
    );
}

#[test]
fn deny_verdict_degradation_is_surfaced() {
    let mut conn = sample_connection();
    conn.dst_host = "shop.co.uk".to_string(); // would degrade
    let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Deny, &conn);
    assert!(
        reason.is_some(),
        "Deny degradation must be surfaced — silent under-blocking is a defect"
    );
}

#[test]
fn deny_verdict_with_no_degradation_reports_none() {
    let conn = sample_connection(); // ThisHost never degrades
    let reason = scope_degradation(VerdictScope::ThisHost, Verdict::Deny, &conn);
    assert!(reason.is_none());
}

/// Issue #44: the once-only reply for a connection without an absolute
/// path still narrows an `AnyHost` deny to this host, and says so.
#[test]
fn deny_verdict_any_host_degradation_is_surfaced_without_an_absolute_path() {
    let mut conn = sample_connection();
    for path in ["", "Kernel connection"] {
        conn.process_path = path.to_string();
        let reason = scope_degradation(VerdictScope::AnyHost, Verdict::Deny, &conn);
        assert_eq!(
            reason,
            Some(ScopeDegradation::ProcessPathUnavailable),
            "{path:?}"
        );
    }
}

// -- FIX 1: rule name sanitization ------------------------------------

#[test]
fn rule_name_for_neutralizes_path_traversal() {
    let name = rule_name_for(Verdict::Allow, "../../../../etc/cron.d/x", 443, "");
    // The security property that actually matters: no `/` survives, so
    // `filepath.Join(l.Path, name + ".json")` can never treat the
    // sanitized name as more than one path component — there's no
    // separator left to walk up directories with. A leftover `..`
    // *substring* inside a single path component (no `/` around it) is
    // an ordinary, harmless filename character sequence on Linux; only
    // an exact `..` *component* (bounded by `/` or string edges after
    // path splitting) is special, and that can't happen here.
    assert!(
        !name.contains('/'),
        "rule name must never contain a path separator: {name}"
    );
}

#[test]
fn rule_name_for_handles_empty_host() {
    let name = rule_name_for(Verdict::Allow, "", 443, "");
    assert!(!name.is_empty());
    assert!(!name.contains('/'));
}

#[test]
fn rule_name_for_is_deterministic_for_the_same_input() {
    let a = rule_name_for(Verdict::Deny, "../../etc/passwd", 80, "");
    let b = rule_name_for(Verdict::Deny, "../../etc/passwd", 80, "");
    assert_eq!(a, b);
}

#[test]
fn rule_name_for_distinct_hostile_hosts_do_not_collide() {
    let a = rule_name_for(Verdict::Deny, "../../etc/passwd", 80, "");
    let b = rule_name_for(Verdict::Deny, "../../etc/shadow", 80, "");
    assert_ne!(a, b, "distinct hostile inputs must not collide");
}

// -- MEDIUM-2 (issue #14 security review round 2): sanitization must be
// injective, or a persisted Deny/Always rule for one host could be
// silently overwritten by a later verdict for a colliding host. -------

#[test]
fn rule_name_for_distinguishes_hosts_that_collide_under_character_mapping_alone() {
    // "_dmarc.example.com", "/dmarc.example.com", and
    // "%dmarc.example.com" all sanitize to the identical string under
    // character-mapping alone (both `/` and `%` map to `_`, same as a
    // literal leading `_`) — the always-appended raw-input digest is
    // what actually distinguishes them.
    let a = rule_name_for(Verdict::Deny, "_dmarc.example.com", 443, "");
    let b = rule_name_for(Verdict::Deny, "/dmarc.example.com", 443, "");
    let c = rule_name_for(Verdict::Deny, "%dmarc.example.com", 443, "");
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
}

#[test]
fn rule_name_for_distinguishes_dot_dash_variants_that_trim_identically() {
    // "evil.com", "evil.com.", and "evil.com--" all trim to the same
    // "evil.com" under leading/trailing `.`/`-` trimming alone.
    let a = rule_name_for(Verdict::Deny, "evil.com", 443, "");
    let b = rule_name_for(Verdict::Deny, "evil.com.", 443, "");
    let c = rule_name_for(Verdict::Deny, "evil.com--", 443, "");
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
}
