use super::shadow::{analyze, Analysis, Finding, FindingKind, MAX_ANALYZED_RULES};
use super::testkit::*;
use crate::rules::row_store::Rule;

fn findings(rules: &[Rule]) -> std::collections::BTreeMap<String, Finding> {
    match analyze(rules) {
        Analysis::Done { findings } => findings,
        other => panic!("expected Done, got {other:?}"),
    }
}

fn only(rules: &[Rule], name: &str) -> Option<(FindingKind, String)> {
    findings(rules).get(name).map(|f| (f.kind, f.by.clone()))
}

fn x() -> serde_json::Value {
    host("example.com")
}

#[test]
fn a_later_deny_shadows_an_earlier_allow() {
    let rules = [allow("100-allow", x()), deny("200-deny", x())];
    assert_eq!(
        only(&rules, "100-allow"),
        Some((FindingKind::NeverApplies, "200-deny".into()))
    );
    assert_eq!(only(&rules, "200-deny"), None);
}

#[test]
fn a_later_allow_shadows_an_earlier_allow_and_an_earlier_one_does_not() {
    let rules = [allow("100-first", x()), allow("200-second", x())];
    assert_eq!(
        only(&rules, "100-first"),
        Some((FindingKind::Redundant, "200-second".into()))
    );
    assert_eq!(only(&rules, "200-second"), None, "the last allow decides");
}

#[test]
fn an_earlier_deny_shadows_a_later_allow_and_a_later_deny_does_not_shadow_it() {
    let rules = [deny("100-deny", x()), allow("200-allow", x())];
    assert_eq!(
        only(&rules, "200-allow"),
        Some((FindingKind::NeverApplies, "100-deny".into()))
    );
    assert_eq!(only(&rules, "100-deny"), None);
}

#[test]
fn a_deny_is_shadowed_only_by_an_earlier_stop_rule() {
    let rules = [deny("100-deny", x()), deny("200-deny", x())];
    assert_eq!(
        only(&rules, "200-deny"),
        Some((FindingKind::Redundant, "100-deny".into()))
    );
    assert_eq!(only(&rules, "100-deny"), None);
}

#[test]
fn a_precedence_allow_before_a_deny_shadows_it_and_a_deny_after_does_not_shadow_it() {
    let mut precedence = allow("100-precedence", x());
    precedence.precedence = true;
    let rules = [precedence, deny("200-deny", x())];
    assert_eq!(
        only(&rules, "200-deny"),
        Some((FindingKind::NeverApplies, "100-precedence".into()))
    );
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
    assert_eq!(
        only(&rules, "200-precedence"),
        Some((FindingKind::NeverApplies, "100-deny".into()))
    );
}

#[test]
fn rules_are_ordered_by_name_whatever_order_they_arrive_in() {
    let rules = [deny("200-deny", x()), allow("100-allow", x())];
    assert_eq!(
        only(&rules, "100-allow"),
        Some((FindingKind::NeverApplies, "200-deny".into()))
    );
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
/// name order, not the arrival order, decides which one is redundant.
#[test]
fn which_allow_is_redundant_follows_the_name_order_not_the_arrival_order() {
    let rules = [allow("200-second", x()), allow("100-first", x())];
    assert_eq!(
        only(&rules, "100-first"),
        Some((FindingKind::Redundant, "200-second".into()))
    );
    assert_eq!(only(&rules, "200-second"), None);
    let rules = [deny("200-second", x()), deny("100-first", x())];
    assert_eq!(
        only(&rules, "200-second"),
        Some((FindingKind::Redundant, "100-first".into()))
    );
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
    assert_eq!(
        only(&rules, "200-narrow"),
        Some((FindingKind::NeverApplies, "100-wide".into()))
    );
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
    assert_eq!(found["100-a"].kind, FindingKind::NeverApplies);
    assert_eq!(found["200-b"].kind, FindingKind::Redundant);
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
    let mut temporary = allow("200-allow", x());
    temporary.duration = "5m".into();
    let rules = [deny("100-deny", x()), temporary];
    assert!(findings(&rules).contains_key("200-allow"));
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
    assert_eq!(
        only(&rules, "100-user"),
        Some((FindingKind::NeverApplies, "050-managed".into()))
    );
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
    assert_eq!(found["300-literal"].kind, FindingKind::NeverApplies);
    assert_eq!(found["300-literal"].by, "200-exact");
    // The deny after the pattern is only possibly shadowed.
    assert_eq!(found["200-exact"].kind, FindingKind::MayBeShadowed);
}

#[test]
fn a_rule_whose_action_the_daemon_treats_as_deny_counts_as_deny() {
    // Not allow/deny/reject: it doesn't stop the scan, and it drops.
    let odd = rule("200-odd", "drop", x());
    let rules = [allow("100-allow", x()), odd];
    assert_eq!(
        only(&rules, "100-allow"),
        Some((FindingKind::NeverApplies, "200-odd".into()))
    );
}

#[test]
fn identical_rules_flag_the_earlier_stop_rule_only_once() {
    let rules = [deny("100-a", x()), deny("200-b", x()), deny("300-c", x())];
    let found = findings(&rules);
    assert_eq!(found["200-b"].by, "100-a");
    assert_eq!(found["300-c"].by, "100-a", "the earliest that covers it");
    assert!(!found.contains_key("100-a"));
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
fn finding_text_says_what_was_found_and_never_that_anything_was_changed() {
    let f = |kind| Finding {
        kind,
        by: "b".into(),
        by_display: "B rule".into(),
    };
    assert_eq!(
        f(FindingKind::Redundant).text(),
        "Redundant: B rule already decides these connections the same way."
    );
    assert_eq!(
        f(FindingKind::NeverApplies).text(),
        "Never applies: B rule decides these connections instead."
    );
    assert_eq!(
        f(FindingKind::MayBeShadowed).text(),
        "May be shadowed by B rule."
    );
    for kind in [
        FindingKind::Redundant,
        FindingKind::NeverApplies,
        FindingKind::MayBeShadowed,
    ] {
        let text = f(kind).text().to_lowercase();
        for word in ["removed", "deleted", "disabled", "changed", "fixed"] {
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
