use std::sync::atomic::{AtomicBool, Ordering};

use super::shadow::{analyze, analyze_until, Analysis, Finding, FindingKind, MAX_ANALYZED_RULES};
use super::testkit::*;
use crate::rules::row_store::Rule;

fn findings(rules: &[Rule]) -> std::collections::BTreeMap<String, Finding> {
    match analyze(rules) {
        Analysis::Done { findings } => findings,
        other => panic!("expected Done, got {other:?}"),
    }
}

/// The kind and the covering rule named for `name`, if it has a finding.
fn only(rules: &[Rule], name: &str) -> Option<(FindingKind, String)> {
    findings(rules).get(name).map(|f| (f.kind, f.by.clone()))
}

/// Just who is named for `name`.
fn named(rules: &[Rule], name: &str) -> Option<String> {
    only(rules, name).map(|(_, by)| by)
}

fn x() -> serde_json::Value {
    host("example.com")
}

fn never(by: &str) -> Option<(FindingKind, String)> {
    Some((FindingKind::NeverDecides, by.to_string()))
}

#[test]
fn a_later_deny_shadows_an_earlier_allow() {
    let rules = [allow("100-allow", x()), deny("200-deny", x())];
    assert_eq!(only(&rules, "100-allow"), never("200-deny"));
    assert_eq!(only(&rules, "200-deny"), None);
}

#[test]
fn a_later_allow_shadows_an_earlier_allow_and_an_earlier_one_does_not() {
    let rules = [allow("100-first", x()), allow("200-second", x())];
    assert_eq!(only(&rules, "100-first"), never("200-second"));
    assert_eq!(only(&rules, "200-second"), None, "the last allow decides");
}

#[test]
fn an_earlier_deny_shadows_a_later_allow_and_a_later_deny_does_not_shadow_it() {
    let rules = [deny("100-deny", x()), allow("200-allow", x())];
    assert_eq!(only(&rules, "200-allow"), never("100-deny"));
    assert_eq!(only(&rules, "100-deny"), None);
}

#[test]
fn a_deny_is_shadowed_only_by_an_earlier_stop_rule() {
    let rules = [deny("100-deny", x()), deny("200-deny", x())];
    assert_eq!(only(&rules, "200-deny"), never("100-deny"));
    assert_eq!(only(&rules, "100-deny"), None);
}

#[test]
fn a_precedence_allow_before_a_deny_shadows_it_and_a_deny_after_does_not_shadow_it() {
    let mut precedence = allow("100-precedence", x());
    precedence.precedence = true;
    let rules = [precedence, deny("200-deny", x())];
    assert_eq!(only(&rules, "200-deny"), never("100-precedence"));
    assert_eq!(
        only(&rules, "100-precedence"),
        None,
        "it stops the scan before the deny"
    );
}

#[test]
fn a_precedence_allow_after_a_deny_does_not_shadow_it_either() {
    let mut precedence = allow("200-precedence", x());
    precedence.precedence = true;
    let rules = [deny("100-deny", x()), precedence];
    assert_eq!(only(&rules, "100-deny"), None);
    assert_eq!(only(&rules, "200-precedence"), never("100-deny"));
}

#[test]
fn a_reject_stops_the_scan_like_a_deny() {
    let rules = [allow("100-allow", x()), rule("200-reject", "reject", x())];
    assert_eq!(only(&rules, "100-allow"), never("200-reject"));
    let rules = [rule("100-reject", "reject", x()), deny("200-deny", x())];
    assert_eq!(only(&rules, "200-deny"), never("100-reject"));
}

/// Only "the first never decides" is proven by the later allow; the deny
/// after it decides these connections, so naming the allow (and calling it
/// "the same way") would be wrong.
#[test]
fn a_covering_stop_rule_is_named_ahead_of_a_later_covering_allow() {
    let rules = [allow("100-b", x()), allow("200-a", x()), deny("300-d", x())];
    assert_eq!(only(&rules, "100-b"), never("300-d"));
    assert_eq!(only(&rules, "200-a"), never("300-d"));
    assert_eq!(only(&rules, "300-d"), None);
}

#[test]
fn the_earliest_covering_stop_rule_is_named_for_an_allow() {
    let rules = [
        deny("050-early", x()),
        allow("100-b", x()),
        deny("200-late", x()),
    ];
    assert_eq!(named(&rules, "100-b").as_deref(), Some("050-early"));
}

#[test]
fn without_a_stop_rule_the_last_covering_allow_is_named() {
    let rules = [
        allow("100-b", x()),
        allow("150-middle", x()),
        allow("200-last", x()),
    ];
    assert_eq!(named(&rules, "100-b").as_deref(), Some("200-last"));
    assert_eq!(named(&rules, "150-middle").as_deref(), Some("200-last"));
    assert_eq!(named(&rules, "200-last"), None);
}

#[test]
fn the_earliest_earlier_stop_rule_is_named_for_a_stop_rule() {
    let rules = [deny("100-a", x()), deny("200-b", x()), deny("300-c", x())];
    assert_eq!(named(&rules, "200-b").as_deref(), Some("100-a"));
    assert_eq!(named(&rules, "300-c").as_deref(), Some("100-a"));
}

/// A rule that matches only part of what the shadowed one does can decide
/// those connections, so the finding must not say who decides, or that the
/// verdict is the same.
#[test]
fn a_partial_rule_in_between_does_not_make_the_claim_wrong() {
    let mut partial = allow(
        "150-partial",
        all_of(vec![host("example.com"), simple("dest.port", "80")]),
    );
    partial.precedence = true;
    let rules = [allow("100-b", x()), partial, deny("200-d", x())];
    let found = findings(&rules);
    assert_eq!(found["100-b"].by, "200-d");
    let text = found["100-b"].text();
    assert!(!text.contains("same way"), "{text}");
    assert!(!text.to_lowercase().contains("decides these"), "{text}");
    assert!(
        !found.contains_key("150-partial"),
        "it still decides port 80"
    );
}

#[test]
fn rules_are_ordered_by_name_whatever_order_they_arrive_in() {
    let rules = [deny("200-deny", x()), allow("100-allow", x())];
    assert_eq!(only(&rules, "100-allow"), never("200-deny"));
    // Byte order: uppercase sorts before lowercase.
    let rules = [allow("a-allow", x()), deny("B-deny", x())];
    assert_eq!(
        only(&rules, "B-deny"),
        None,
        "B sorts before a, so the deny comes first and shadows the allow"
    );
    assert!(only(&rules, "a-allow").is_some());
}

/// Order matters between two allows (the later replaces the earlier), so the
/// name order, not the arrival order, decides which one never decides.
#[test]
fn which_allow_never_decides_follows_the_name_order_not_the_arrival_order() {
    let rules = [allow("200-second", x()), allow("100-first", x())];
    assert_eq!(only(&rules, "100-first"), never("200-second"));
    assert_eq!(only(&rules, "200-second"), None);
    let rules = [deny("200-second", x()), deny("100-first", x())];
    assert_eq!(only(&rules, "200-second"), never("100-first"));
    assert_eq!(only(&rules, "100-first"), None);
}

#[test]
fn a_wider_rule_shadows_a_narrower_one_but_not_the_other_way() {
    let wide = deny("100-wide", host("example.com"));
    let narrow = allow(
        "200-narrow",
        all_of(vec![host("example.com"), simple("dest.port", "443")]),
    );
    let rules = [wide, narrow];
    assert_eq!(only(&rules, "200-narrow"), never("100-wide"));
    assert_eq!(only(&rules, "100-wide"), None);
}

#[test]
fn a_rule_that_matches_everything_shadows_all_that_follow() {
    let rules = [
        deny("000-everything", truth()),
        allow("100-a", host("a.example.com")),
        deny("200-b", simple("process.path", "/usr/bin/curl")),
    ];
    let found = findings(&rules);
    assert_eq!(found["100-a"].by, "000-everything");
    assert_eq!(found["200-b"].by, "000-everything");
    assert!(!found.contains_key("000-everything"));
}

#[test]
fn a_rule_that_does_not_last_does_not_shadow_anything() {
    for duration in ["5m", "until restart", "once", "1h30m", ""] {
        let mut deny_rule = deny("100-deny", x());
        deny_rule.duration = duration.to_string();
        let rules = [deny_rule, allow("200-allow", x())];
        assert!(
            findings(&rules).is_empty(),
            "a rule with duration {duration:?} is not permanent"
        );
    }
    // The shadowed rule may be temporary.
    for duration in ["5m", "until restart"] {
        let mut temporary = allow("200-allow", x());
        temporary.duration = duration.into();
        let rules = [deny("100-deny", x()), temporary];
        assert!(findings(&rules).contains_key("200-allow"), "{duration}");
    }
}

#[test]
fn disabled_rules_neither_shadow_nor_get_findings() {
    let mut off = deny("100-deny", x());
    off.enabled = false;
    let rules = [off, allow("200-allow", x())];
    assert!(findings(&rules).is_empty());
    let mut off = allow("200-allow", x());
    off.enabled = false;
    let rules = [deny("100-deny", x()), off];
    assert!(findings(&rules).is_empty());
}

#[test]
fn rules_managed_elsewhere_get_no_finding_but_may_shadow_user_rules() {
    let mut read_only = allow("200-locked", x());
    read_only.read_only_reason = Some("managed".into());
    let packaged = allow("200-packaged", x());
    let packaged = Rule {
        name: "000-snitchwatch-bridge-fetch".into(),
        ..packaged
    };
    let blocklist = deny("z00-blocklist:ads:domains", x());
    let rules = [deny("100-deny", x()), read_only, packaged, blocklist];
    assert!(findings(&rules).is_empty(), "{:?}", findings(&rules));

    // A managed rule can still be the one that shadows.
    let mut managed_deny = deny("050-managed", x());
    managed_deny.read_only_reason = Some("managed".into());
    let rules = [managed_deny, allow("100-user", x())];
    assert_eq!(only(&rules, "100-user"), never("050-managed"));
}

/// A profile's rules (`850-profile:`) are installed while the profile is active
/// and removed by the next network switch, so a user rule is never said to be
/// shadowed by one, whichever way round the actions are.
#[test]
fn a_profile_rule_is_never_named_as_the_shadowing_rule() {
    let rules = [deny("850-profile:work:r1", x()), allow("900-user", x())];
    assert!(findings(&rules).is_empty(), "{:?}", findings(&rules));
    let rules = [allow("100-user", x()), allow("850-profile:work:r1", x())];
    assert!(findings(&rules).is_empty(), "{:?}", findings(&rules));

    // The user's own covering rule is still named, profile rule or not.
    let rules = [
        deny("850-profile:work:r1", x()),
        deny("500-user-deny", x()),
        allow("900-user", x()),
    ];
    assert_eq!(only(&rules, "900-user"), never("500-user-deny"));
}

#[test]
fn a_rule_with_a_condition_it_cannot_compare_shadows_nothing() {
    let opaque = deny(
        "100-opaque",
        all_of(vec![host("example.com"), simple("user.name", "bob")]),
    );
    let rules = [opaque, allow("200-allow", x())];
    assert!(findings(&rules).is_empty());
    // But it can be shadowed by a rule covering its comparable part.
    let rules = [
        deny("100-deny", x()),
        allow(
            "200-opaque",
            all_of(vec![host("example.com"), simple("user.name", "bob")]),
        ),
    ];
    assert!(findings(&rules).contains_key("200-opaque"));
}

#[test]
fn a_proof_that_needs_the_regexp_engine_says_may_be_shadowed() {
    let rules = [
        deny("100-pattern", regexp("dest.host", r"^.*\.example\.com$")),
        allow(
            "200-literal",
            simple_sensitive("dest.host", "a.example.com"),
        ),
    ];
    assert_eq!(
        only(&rules, "200-literal"),
        Some((FindingKind::MayBeShadowed, "100-pattern".into()))
    );
}

#[test]
fn the_strongest_proof_wins_over_an_earlier_weaker_one() {
    let rules = [
        deny("100-pattern", regexp("dest.host", r"^.*\.example\.com$")),
        deny("200-exact", simple_sensitive("dest.host", "a.example.com")),
        allow(
            "300-literal",
            simple_sensitive("dest.host", "a.example.com"),
        ),
    ];
    let found = findings(&rules);
    assert_eq!(found["300-literal"].kind, FindingKind::NeverDecides);
    assert_eq!(found["300-literal"].by, "200-exact");
    // The deny after the pattern is only possibly shadowed.
    assert_eq!(found["200-exact"].kind, FindingKind::MayBeShadowed);
}

#[test]
fn a_rule_whose_action_the_daemon_treats_as_deny_replaces_an_allow() {
    // Not allow/deny/reject: it doesn't stop the scan, and it drops.
    let odd = rule("200-odd", "drop", x());
    let rules = [allow("100-allow", x()), odd];
    assert_eq!(only(&rules, "100-allow"), never("200-odd"));
}

#[test]
fn the_display_name_of_the_covering_rule_is_what_is_shown() {
    let mut shadow = deny("200-deny", x());
    shadow.display_name = Some("200-deny (shown)".into());
    let rules = [allow("100-allow", x()), shadow];
    let found = findings(&rules);
    assert_eq!(found["100-allow"].by, "200-deny");
    assert_eq!(found["100-allow"].by_display, "200-deny (shown)");
    assert!(found["100-allow"].text().contains("200-deny (shown)"));
}

#[test]
fn finding_text_claims_only_that_the_rule_never_decides_and_who_takes_precedence() {
    let f = |kind| Finding {
        kind,
        by: "b".into(),
        by_display: "B rule".into(),
    };
    assert_eq!(
        f(FindingKind::NeverDecides).text(),
        "Never decides: B rule matches every connection this rule does and takes precedence."
    );
    assert_eq!(
        f(FindingKind::MayBeShadowed).text(),
        "May never decide: B rule appears to match every connection this rule does and take \
         precedence."
    );
    for kind in [FindingKind::NeverDecides, FindingKind::MayBeShadowed] {
        let text = f(kind).text().to_lowercase();
        for word in [
            "removed",
            "deleted",
            "disabled",
            "changed",
            "fixed",
            "same way",
            "redundant",
            "decides these",
        ] {
            assert!(!text.contains(word), "{text}");
        }
    }
}

#[test]
fn too_many_enabled_rules_are_not_analyzed() {
    let many = |n: usize| -> Vec<Rule> {
        (0..n)
            .map(|i| allow(&format!("{i:05}"), host(&format!("h{i}.example.com"))))
            .collect()
    };
    assert!(matches!(
        analyze(&many(MAX_ANALYZED_RULES)),
        Analysis::Done { .. }
    ));
    match analyze(&many(MAX_ANALYZED_RULES + 1)) {
        Analysis::TooMany { enabled, limit } => {
            assert_eq!(enabled, MAX_ANALYZED_RULES + 1);
            assert_eq!(limit, MAX_ANALYZED_RULES);
        }
        other => panic!("expected TooMany, got {other:?}"),
    }
    // Disabled rules don't count toward the cap.
    let mut rules = many(MAX_ANALYZED_RULES);
    rules.extend((0..50).map(|i| {
        let mut r = allow(&format!("z{i:05}"), host("x"));
        r.enabled = false;
        r
    }));
    assert!(matches!(analyze(&rules), Analysis::Done { .. }));
}

#[test]
fn a_cancelled_analysis_stops_and_says_nothing() {
    let rules = [allow("100-a", x()), deny("200-d", x())];
    let cancel = AtomicBool::new(false);
    assert!(analyze_until(&rules, &cancel).is_some());
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(analyze_until(&rules, &cancel), None);
}
