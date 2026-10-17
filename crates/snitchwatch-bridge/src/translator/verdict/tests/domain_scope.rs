use super::*;

#[test]
fn this_host_scope_matches_on_dest_host() {
    let rule = once_rule(
        Verdict::Allow,
        VerdictScope::ThisHost,
        &sample_connection(),
        0,
    );
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple");
    assert_eq!(op.operand, "dest.host");
    assert_eq!(op.data, "github.com");
}

#[test]
fn this_host_scope_falls_back_to_dest_ip_when_host_is_empty() {
    let mut conn = sample_connection();
    conn.dst_host = String::new();
    let rule = once_rule(Verdict::Allow, VerdictScope::ThisHost, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple");
    assert_eq!(op.operand, "dest.ip");
    assert_eq!(op.data, "140.82.121.4");
}

#[test]
fn any_host_on_domain_scope_wildcards_the_leftmost_label() {
    let mut conn = sample_connection();
    conn.dst_host = "www.example.com".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "regexp");
    assert_eq!(op.operand, "dest.host");
    assert_eq!(op.data, r"^(?:[^.]+\.)*example\.com$");
}

#[test]
fn any_host_on_domain_scope_wildcard_matches_bare_apex_and_subdomains() {
    // FIX 3(c): the tightened anchor must match real label boundaries
    // AND the bare parent domain itself (closing the deny-leaves-apex-
    // reachable gap from FIX 2).
    let re = regex::Regex::new(r"^(?:[^.]+\.)*example\.com$").unwrap();
    assert!(re.is_match("example.com"), "must match the bare apex");
    assert!(re.is_match("www.example.com"));
    assert!(re.is_match("a.b.example.com"), "nested subdomains");
    assert!(!re.is_match("notexample.com"));
    assert!(!re.is_match("example.com.evil.com"));
}

#[test]
fn any_host_on_domain_scope_degrades_for_hostname_with_invalid_characters() {
    // `+` is not in the allowed hostname charset ([A-Za-z0-9_-]) —
    // `is_valid_hostname_shape` rejects it, so this degrades to an exact
    // match rather than ever reaching regex-pattern construction. This
    // is what actually prevents regex-metacharacter injection: no valid
    // (post-validation) hostname can contain an unescaped-needing
    // character in the first place, since the allowed charset has none.
    let mut conn = sample_connection();
    conn.dst_host = "a.b+c.example.com".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    assert_eq!(op.operand, "dest.host");
}

#[test]
fn any_host_on_domain_scope_escapes_literal_dots_in_the_parent_domain() {
    // Dots are the one "special" character a valid hostname legitimately
    // contains, and they must be escaped as literal dots, not left as
    // the regex any-character metacharacter.
    let mut conn = sample_connection();
    conn.dst_host = "www.example.co.uk".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    let re = regex::Regex::new(&op.data).unwrap();
    assert!(re.is_match("example.co.uk"));
    assert!(
        !re.is_match("exampleXcoXuk"),
        "dots must be literal, not `.`"
    );
}

#[test]
fn any_host_on_domain_scope_degrades_to_this_host_for_single_label_host() {
    let mut conn = sample_connection();
    conn.dst_host = "localhost".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    // Never a bare wildcard-everything pattern.
    assert_ne!(
        op.r#type, "regexp",
        "must not emit a regexp for a single-label host"
    );
    assert_eq!(op.r#type, "simple");
    assert_eq!(op.operand, "dest.host");
    assert_eq!(op.data, "localhost");
}

#[test]
fn any_host_on_domain_scope_degrades_to_this_host_for_two_label_host() {
    // `example.com` (via the `github.com` fixture) IS its own
    // registrable domain (eTLD+1) — nothing to safely wildcard.
    let conn = sample_connection(); // dst_host = "github.com"
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple");
    assert_eq!(op.operand, "dest.host");
    assert_eq!(op.data, "github.com");
}

#[test]
fn any_host_on_domain_scope_degrades_to_this_host_for_bare_ip() {
    let mut conn = sample_connection();
    conn.dst_host = "203.0.113.5".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = rule.operator.unwrap();
    assert_ne!(
        op.r#type, "regexp",
        "must not emit a regexp for a bare IP host"
    );
}

// -- FIX 3(a): guard-bypass hostname shapes --------------------------

#[test]
fn any_host_on_domain_scope_degrades_for_leading_dot_host() {
    // ".example.com" splits into 3 parts (["", "example", "com"]),
    // which used to slip past a naive `labels.len() < 3` guard.
    let mut conn = sample_connection();
    conn.dst_host = ".example.com".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
}

#[test]
fn any_host_on_domain_scope_degrades_for_trailing_dot_host() {
    // "example.com." would otherwise produce `^.*\.com\.$`-style
    // patterns that match nothing real (a Deny that silently blocks
    // zero traffic).
    let mut conn = sample_connection();
    conn.dst_host = "example.com.".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
}

// -- FIX 3(b): real public-suffix boundaries, not a label-count guess -

#[test]
fn any_host_on_domain_scope_wildcards_multi_label_etld_correctly() {
    let mut conn = sample_connection();
    conn.dst_host = "www.example.co.uk".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "regexp");
    assert_eq!(op.data, r"^(?:[^.]+\.)*example\.co\.uk$");
}

#[test]
fn any_host_on_domain_scope_degrades_when_host_is_exactly_its_etld_plus_one_multi_label() {
    // `co.uk` is a 2-label eTLD; `shop.co.uk` IS the registrable domain
    // (eTLD+1) — nothing safe to wildcard. A label-count-only guard
    // (labels.len() < 3) would have wrongly allowed this.
    let mut conn = sample_connection();
    conn.dst_host = "shop.co.uk".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    assert_eq!(op.data, "shop.co.uk");
}

#[test]
fn any_host_on_domain_scope_degrades_for_private_suffix_github_io() {
    // `github.io` is listed in the real PSL as a *private* suffix
    // (GitHub Pages). `user.github.io` is the registrable domain —
    // wildcarding it would match every GitHub Pages site.
    let mut conn = sample_connection();
    conn.dst_host = "user.github.io".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    assert_eq!(op.data, "user.github.io");
}

// -- CRITICAL: uppercase must not bypass the PSL gate (round 2) -------

#[test]
fn any_host_on_domain_scope_degrades_for_uppercase_private_suffix_host() {
    // Before the fix, `psl`'s suffix lookup missed every real entry on
    // uppercase input, fell through to the default "last label only"
    // rule, and `.prefix()` incorrectly returned `Some` — this would
    // have wildcarded to `^(?:[^.]+\.)*github\.io$`, matching every
    // GitHub Pages site.
    let mut conn = sample_connection();
    conn.dst_host = "USER.GITHUB.IO".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
}

#[test]
fn any_host_on_domain_scope_degrades_for_uppercase_two_label_etld_host() {
    let mut conn = sample_connection();
    conn.dst_host = "SHOP.CO.UK".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
}

#[test]
fn any_host_on_domain_scope_wildcards_mixed_case_host_correctly() {
    // A mixed-case host that legitimately has a subdomain beyond its
    // registrable domain must still wildcard — the fix must not
    // over-correct into degrading every non-lowercase host.
    let mut conn = sample_connection();
    conn.dst_host = "Www.Example.Com".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "regexp");
    assert_eq!(op.data, r"^(?:[^.]+\.)*example\.com$");
}

#[test]
fn any_host_on_domain_scope_wildcards_uppercase_multi_label_etld_correctly() {
    let mut conn = sample_connection();
    conn.dst_host = "WWW.EXAMPLE.CO.UK".to_string();
    let rule = once_rule(Verdict::Allow, VerdictScope::AnyHostOnDomain, &conn, 0);
    let op = process_bound_host_member(rule, "/usr/bin/curl");
    assert_eq!(op.r#type, "regexp");
    assert_eq!(op.data, r"^(?:[^.]+\.)*example\.co\.uk$");
}

#[test]
fn deny_verdict_degradation_is_surfaced_for_uppercase_bypass_host() {
    // Before the fix this returned None (the bypass meant no
    // degradation was ever detected), so the FIX 2 guarantee — a Deny
    // never silently narrows without telling the caller — didn't
    // actually hold for this input. Confirms it does now.
    let mut conn = sample_connection();
    conn.dst_host = "USER.GITHUB.IO".to_string();
    let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Deny, &conn);
    assert!(
        reason.is_some(),
        "uppercase bypass must still be caught and surfaced for Deny"
    );
    assert_eq!(reason, Some(ScopeDegradation::AtOrAboveRegistrableDomain));
}
