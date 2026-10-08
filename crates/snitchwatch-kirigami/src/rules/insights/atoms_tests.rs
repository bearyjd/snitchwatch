use super::atoms::{Conjunction, Proof};
use super::testkit::*;
use crate::rules::row_store::Rule;

/// Whether `a` provably matches every connection `b` matches, and how.
fn covers(a: &Rule, b: &Rule) -> Option<Proof> {
    Conjunction::from_rule(a).covers(&Conjunction::from_rule(b))
}

fn host_rule(operator: serde_json::Value) -> Rule {
    allow("r", operator)
}

#[test]
fn an_insensitive_simple_condition_covers_a_case_variant() {
    let a = host_rule(host("Example.COM"));
    let b = host_rule(host("example.com"));
    assert_eq!(covers(&a, &b), Some(Proof::Exact));
    assert_eq!(covers(&b, &a), Some(Proof::Exact));
    // Sensitive B: its one subject is also in the insensitive class.
    let b = host_rule(simple_sensitive("dest.host", "example.com"));
    assert_eq!(covers(&a, &b), Some(Proof::Exact));
}

#[test]
fn a_sensitive_condition_covers_only_the_exact_same_text_and_never_an_insensitive_one() {
    let sensitive = host_rule(simple_sensitive("dest.host", "example.com"));
    let insensitive = host_rule(host("example.com"));
    assert_eq!(covers(&sensitive, &insensitive), None);
    assert_eq!(covers(&sensitive, &sensitive), Some(Proof::Exact));
    let other_case = host_rule(simple_sensitive("dest.host", "Example.com"));
    assert_eq!(covers(&sensitive, &other_case), None);
}

#[test]
fn go_case_folding_decides_what_is_equal() {
    // U+212A KELVIN SIGN folds with k, so these are one class for the daemon.
    let kelvin = host_rule(host("\u{212A}"));
    let k = host_rule(host("k"));
    assert_eq!(covers(&kelvin, &k), Some(Proof::Exact));
    // ß does not fold with ss.
    assert_eq!(covers(&host_rule(host("ss")), &host_rule(host("ß"))), None);
}

#[test]
fn conditions_on_different_operands_never_imply_each_other() {
    let a = host_rule(simple("dest.host", "example.com"));
    let b = host_rule(simple("process.path", "example.com"));
    assert_eq!(covers(&a, &b), None);
}

#[test]
fn a_regexp_covers_a_literal_it_matches_but_only_by_the_simulators_engine() {
    let a = host_rule(regexp("dest.host", r"^.*\.example\.com$"));
    assert_eq!(
        covers(&a, &host_rule(host("a.example.com"))),
        Some(Proof::Engine)
    );
    assert_eq!(covers(&a, &host_rule(host("example.org"))), None);
    // A literal that is not in the regexp's language is not covered.
    assert_eq!(covers(&a, &host_rule(host("example.com"))), None);
}

#[test]
fn a_sensitive_regexp_does_not_cover_an_insensitive_literal() {
    let a = host_rule(regexp_sensitive("dest.host", r"^a\.example\.com$"));
    let insensitive = host_rule(host("a.example.com"));
    let sensitive = host_rule(simple_sensitive("dest.host", "a.example.com"));
    assert_eq!(
        covers(&a, &insensitive),
        None,
        "A.EXAMPLE.COM also matches B"
    );
    assert_eq!(covers(&a, &sensitive), Some(Proof::Engine));
}

/// Go's folding puts U+017F (long s) with `s`, but `ToLower` leaves it alone,
/// so an insensitive regexp can miss a connection an insensitive literal
/// matches. A literal with an `s` is therefore not proven covered.
#[test]
fn a_literal_with_an_s_is_not_covered_by_an_insensitive_regexp() {
    let a = host_rule(regexp("dest.host", r"^.*\.example\.com$"));
    assert_eq!(covers(&a, &host_rule(host("sub.example.com"))), None);
    let sensitive = host_rule(simple_sensitive("dest.host", "sub.example.com"));
    assert_eq!(covers(&a, &sensitive), Some(Proof::Engine));
    // Non-ASCII literals fold in ways the lowercase doesn't follow.
    assert_eq!(covers(&a, &host_rule(host("ü.example.com"))), None);
}

#[test]
fn a_pattern_the_simulator_cannot_read_covers_nothing() {
    let outside_the_allowlist = host_rule(regexp("dest.host", "[a-]"));
    let broken = host_rule(regexp("dest.host", "("));
    for a in [outside_the_allowlist, broken] {
        assert_eq!(covers(&a, &host_rule(host("a"))), None);
    }
}

#[test]
fn identical_regexps_cover_each_other_and_different_ones_do_not() {
    let a = host_rule(regexp("dest.host", r"^a+$"));
    assert_eq!(covers(&a, &a.clone()), Some(Proof::Exact));
    let b = host_rule(regexp("dest.host", r"^a*$"));
    assert_eq!(covers(&a, &b), None);
    assert_eq!(
        covers(&b, &a),
        None,
        "no containment proofs between patterns"
    );
    // Same text but a different case rule is a different function.
    let sensitive = host_rule(regexp_sensitive("dest.host", r"^a+$"));
    assert_eq!(covers(&a, &sensitive), None);
}

#[test]
fn a_true_rule_covers_everything_and_nothing_covers_it_but_another_true() {
    let all = host_rule(truth());
    for b in [
        host_rule(host("example.com")),
        host_rule(simple("user.name", "bob")),
        host_rule(serde_json::Value::Null),
        host_rule(all_of(vec![])),
    ] {
        assert_eq!(covers(&all, &b), Some(Proof::Exact));
    }
    assert_eq!(covers(&host_rule(host("example.com")), &all), None);
    assert_eq!(covers(&all, &all), Some(Proof::Exact));
    // A `true` member of a list adds no condition.
    let with_true = host_rule(all_of(vec![truth(), host("example.com")]));
    assert_eq!(
        covers(&with_true, &host_rule(host("example.com"))),
        Some(Proof::Exact)
    );
}

#[test]
fn a_network_covers_a_literal_address_and_a_narrower_network() {
    let lan = host_rule(network("dest.network", "10.0.0.0/8"));
    assert_eq!(
        covers(&lan, &host_rule(simple("dest.ip", "10.1.2.3"))),
        Some(Proof::Exact)
    );
    assert_eq!(
        covers(&lan, &host_rule(network("dest.network", "10.1.0.0/16"))),
        Some(Proof::Exact)
    );
    assert_eq!(covers(&lan, &lan.clone()), Some(Proof::Exact));
    // Outside, wider, and a different direction.
    assert_eq!(
        covers(&lan, &host_rule(simple("dest.ip", "11.1.2.3"))),
        None
    );
    assert_eq!(
        covers(&lan, &host_rule(network("dest.network", "0.0.0.0/0"))),
        None
    );
    assert_eq!(
        covers(&lan, &host_rule(simple("source.ip", "10.1.2.3"))),
        None
    );
    assert_eq!(
        covers(&host_rule(network("dest.network", "10.1.0.0/16")), &lan),
        None,
        "a narrower network does not cover a wider one"
    );
}

#[test]
fn host_bits_in_a_cidr_are_masked_like_the_daemon_does() {
    let a = host_rule(network("dest.network", "10.1.2.3/8"));
    assert_eq!(
        covers(&a, &host_rule(simple("dest.ip", "10.200.0.1"))),
        Some(Proof::Exact)
    );
}

#[test]
fn only_exact_ipv4_literals_and_literal_cidrs_are_compared() {
    let lan = host_rule(network("dest.network", "10.0.0.0/8"));
    for odd in ["010.1.2.3", "::ffff:10.1.2.3", "10.1.2", "ten", ""] {
        assert_eq!(
            covers(&lan, &host_rule(simple("dest.ip", odd))),
            None,
            "{odd}"
        );
    }
    // An alias is whatever the daemon host's alias file says: it covers
    // only the very same alias.
    let alias = host_rule(network("dest.network", "LAN"));
    assert_eq!(covers(&alias, &lan), None);
    assert_eq!(covers(&lan, &alias), None);
    assert_eq!(covers(&alias, &alias.clone()), Some(Proof::Exact));
    // IPv6 networks are compared only with themselves.
    let v6 = host_rule(network("dest.network", "fd00::/8"));
    assert_eq!(covers(&v6, &host_rule(simple("dest.ip", "fd00::1"))), None);
    assert_eq!(covers(&v6, &v6.clone()), Some(Proof::Exact));
    // A network type on a text operand, or the reverse, isn't a rule the
    // daemon loads.
    let bad = host_rule(network("dest.host", "10.0.0.0/8"));
    assert_eq!(covers(&bad, &bad.clone()), None);
}

#[test]
fn a_condition_the_analysis_does_not_model_never_covers_anything() {
    for operator in [
        simple("user.name", "bob"),
        simple("iface.in", "eth0"),
        simple("process.hash.md5", "abc"),
        simple("process.env.HOME", "/home/bob"),
        simple("process.parent.path", "/usr/bin/bash"),
        serde_json::json!({"type": "lists", "operand": "lists.domains", "data": "/x"}),
        serde_json::json!({"type": "weird", "operand": "dest.host", "data": "x"}),
        serde_json::Value::Null,
        all_of(vec![]),
        all_of(vec![all_of(vec![host("example.com")])]),
    ] {
        let a = host_rule(operator);
        assert_eq!(
            covers(&a, &a.clone()),
            None,
            "{:?} must not cover even itself",
            a.operator
        );
    }
}

#[test]
fn conditions_the_other_rule_has_are_still_premises_when_some_are_unmodelled() {
    let a = host_rule(host("example.com"));
    let b = host_rule(all_of(vec![
        host("example.com"),
        simple("process.hash.md5", "abc"),
        simple("user.name", "bob"),
    ]));
    assert_eq!(covers(&a, &b), Some(Proof::Exact));
}

#[test]
fn every_condition_of_the_covering_rule_must_be_implied() {
    let narrow = host_rule(all_of(vec![
        host("example.com"),
        simple("dest.port", "443"),
        simple("protocol", "tcp"),
    ]));
    let wide = host_rule(all_of(vec![
        host("example.com"),
        simple("dest.port", "443"),
    ]));
    // The two-condition rule covers the three-condition one, not the reverse.
    assert_eq!(covers(&wide, &narrow), Some(Proof::Exact));
    assert_eq!(covers(&narrow, &wide), None);
    // Order does not matter.
    let reordered = host_rule(all_of(vec![
        simple("protocol", "tcp"),
        simple("dest.port", "443"),
        host("EXAMPLE.com"),
    ]));
    assert_eq!(covers(&reordered, &narrow), Some(Proof::Exact));
}

#[test]
fn the_weakest_proof_in_a_rule_is_the_proof_of_the_rule() {
    let a = host_rule(all_of(vec![
        regexp("dest.host", r"^.*\.example\.com$"),
        simple("dest.port", "443"),
    ]));
    let b = host_rule(all_of(vec![
        simple_sensitive("dest.host", "a.example.com"),
        simple("dest.port", "443"),
    ]));
    assert_eq!(covers(&a, &b), Some(Proof::Engine));
}
