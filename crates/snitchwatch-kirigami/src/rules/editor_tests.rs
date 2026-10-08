//! Tests for [`super::editor`]: the draft model, its wire shape, what is
//! editable, and what the sheet warns about.

use super::editor::*;
use super::simulator::SimulationForm;
use serde_json::{json, Value};

fn condition(operand: &str, kind: MatchKind, value: &str) -> Condition {
    Condition {
        operand: operand.into(),
        kind,
        value: value.into(),
        case_sensitive: false,
    }
}

fn draft(conditions: Vec<Condition>) -> RuleDraft {
    RuleDraft {
        name: "899-written".into(),
        description: String::new(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        precedence: false,
        nolog: false,
        conditions,
    }
}

fn path(value: &str) -> Condition {
    Condition {
        case_sensitive: true,
        ..condition("process.path", MatchKind::Exact, value)
    }
}

#[test]
fn one_condition_is_a_leaf_and_more_are_a_list() {
    let one = draft(vec![path("/usr/bin/curl")]).to_wire();
    assert_eq!(one["operator"]["type"], "simple");
    assert_eq!(one["operator"]["operand"], "process.path");
    assert_eq!(one["operator"]["sensitive"], true);
    let two = draft(vec![
        path("/usr/bin/curl"),
        condition("dest.host", MatchKind::Pattern, "^example\\.com$"),
    ])
    .to_wire();
    assert_eq!(two["operator"]["type"], "list");
    assert_eq!(two["operator"]["operand"], "list");
    assert_eq!(two["operator"]["operands"][1]["type"], "regexp");
}

#[test]
fn every_offered_operand_and_kind_round_trips() {
    for operand in operands() {
        for kind in &operand.kinds {
            let value = match (operand.operand, kind) {
                (_, MatchKind::Network) => "10.0.0.0/8",
                (_, MatchKind::Pattern) => "^ab",
                ("process.path" | "process.parent.path", _) => "/usr/bin/curl",
                ("dest.port" | "source.port", _) => "443",
                ("user.id", _) => "1000",
                ("dest.ip" | "source.ip", _) => "10.0.0.1",
                ("protocol", _) => "tcp",
                _ => "abc",
            };
            let original = draft(vec![
                path("/usr/bin/curl"),
                condition(operand.operand, *kind, value),
            ]);
            let wire = original.to_wire();
            let back = RuleDraft::from_wire(&wire)
                .unwrap_or_else(|e| panic!("{} {kind:?}: {e}", operand.operand));
            assert_eq!(back, original, "{} {kind:?}", operand.operand);
        }
    }
}

#[test]
fn match_kinds_follow_the_operand() {
    let kinds = |name: &str| {
        operands()
            .into_iter()
            .find(|o| o.operand == name)
            .expect(name)
            .kinds
    };
    assert_eq!(kinds("dest.network"), vec![MatchKind::Network]);
    assert_eq!(kinds("source.network"), vec![MatchKind::Network]);
    assert!(!kinds("dest.ip").contains(&MatchKind::Network));
    assert!(!kinds("process.path").contains(&MatchKind::Network));
    for never in ["true", "list", "process.hash.md5", "process.id"] {
        assert!(operands().iter().all(|o| o.operand != never), "{never}");
    }
}

#[test]
fn rules_the_editor_cant_express_are_not_editable() {
    let wire = |operator: Value| {
        json!({ "name": "899-x", "enabled": true, "action": "deny", "duration": "always",
                "operator": operator })
    };
    let leaf = |t: &str, o: &str, d: &str| json!({ "type": t, "operand": o, "data": d });
    let path = leaf("simple", "process.path", "/usr/bin/curl");
    for operator in [
        json!({ "type": "list", "operands": [{ "type": "list", "operands": [path.clone()] }] }),
        json!({ "type": "list", "operands": [path.clone(), leaf("lists", "lists.nets", "/x")] }),
        json!({ "type": "list", "operands": [path.clone(),
                leaf("simple", "process.hash.sha1", "da39a3ee5e6b4b0d3255bfef95601890afd80709")] }),
        leaf("complex", "dest.host", "x"),
        leaf("network", "dest.ip", "10.0.0.0/8"),
        leaf("simple", "process.path", "curl"),
        json!({ "type": "list", "operands": [path, leaf("simple", "process.id", "42")] }),
    ] {
        let reason = RuleDraft::from_wire(&wire(operator.clone())).unwrap_err();
        assert!(!reason.is_empty(), "{operator}");
    }
}

#[test]
fn suggested_names_are_always_valid() {
    for host in [
        "../../etc/passwd",
        "a\u{202e}b.example",
        "<b>x</b>",
        &"a".repeat(300),
        "",
    ] {
        let mut d = draft(vec![condition("dest.host", MatchKind::Exact, host)]);
        d.name.clear();
        let name = suggest_name(&d);
        assert!(
            snitchwatch_bridge::rule_name::validate_rule_name(&name).is_ok(),
            "{name}"
        );
        assert!(name.starts_with("snitchwatch-deny-"), "{name}");
        assert!(!snitchwatch_bridge::rule_name::is_reserved_name(&name));
    }
}

#[test]
fn the_all_programs_warning_shows_exactly_without_a_program_condition() {
    let host = condition("dest.host", MatchKind::Exact, "example.com");
    let warns = |d: &RuleDraft| check(d, None).warnings;
    assert!(warns(&draft(vec![host.clone()]))
        .iter()
        .any(|w| w.contains("every program")));
    assert!(!warns(&draft(vec![path("/usr/bin/curl"), host.clone()]))
        .iter()
        .any(|w| w.contains("every program")));
    // A path pattern (E2) doesn't tie a rule to programs either, and warns.
    let pattern = draft(vec![
        condition("process.path", MatchKind::Pattern, "^/home/[^/]+/\\.steam/"),
        host,
    ]);
    let warnings = warns(&pattern);
    assert!(
        warnings.iter().any(|w| w.contains("every program")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("more programs than you mean")),
        "{warnings:?}"
    );
}

#[test]
fn flags_and_durations_warn_in_plain_words() {
    let mut d = draft(vec![path("/usr/bin/curl")]);
    d.precedence = true;
    d.nolog = true;
    d.duration = "5m".into();
    let warnings = check(&d, None).warnings;
    for needle in [
        "Decides before other rules",
        "Hides this rule's connections",
        "Lost when the firewall restarts",
    ] {
        assert!(
            warnings.iter().any(|w| w.contains(needle)),
            "{needle}: {warnings:?}"
        );
    }
    for raw in ["nolog", "precedence"] {
        assert!(warnings.iter().all(|w| !w.contains(raw)), "{warnings:?}");
    }
}

#[test]
fn problems_and_cautions_come_from_the_bridge() {
    let mut bad = draft(vec![path("curl")]);
    bad.duration = "1.5h".into();
    let problems = check(&bad, None).problems;
    assert!(
        problems.iter().any(|p| p.contains("full path")),
        "{problems:?}"
    );
    assert!(problems.iter().any(|p| p.contains("1h30m")), "{problems:?}");
    // Problem locations are plain words, never field paths.
    assert!(
        problems.iter().all(|p| !p.contains("operator")),
        "{problems:?}"
    );

    let old = draft(vec![path("/usr/bin/curl")]).to_wire();
    let mut loosened = draft(vec![path("/usr/bin/curl")]);
    loosened.action = "allow".into();
    let cautions = check(&loosened, Some(&old)).cautions;
    assert!(
        cautions.iter().any(|c| c.contains("into an allow")),
        "{cautions:?}"
    );
    let fresh = check(&loosened, None).cautions;
    assert!(
        fresh.iter().all(|c| !c.contains("into an allow")),
        "{fresh:?}"
    );
}

#[test]
fn a_connection_prefills_only_what_identifies_it() {
    let form = SimulationForm {
        process_path: "/usr/bin/curl".into(),
        dest_host: "example.com".into(),
        dest_ip: "93.184.216.34".into(),
        dest_port: 443,
        protocol: "tcp".into(),
        ..Default::default()
    };
    let d = prefill(&form);
    let pairs: Vec<_> = d
        .conditions
        .iter()
        .map(|c| (c.operand.as_str(), c.value.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("process.path", "/usr/bin/curl"),
            ("dest.host", "example.com"),
            ("dest.port", "443")
        ]
    );
    assert!(d.conditions[0].case_sensitive);
    assert!(snitchwatch_bridge::rule_name::validate_rule_name(&d.name).is_ok());
    let bare = prefill(&SimulationForm {
        process_path: "Kernel connection".into(),
        dest_host_empty: true,
        dest_ip: "10.0.0.1".into(),
        ..Default::default()
    });
    let operands: Vec<_> = bare.conditions.iter().map(|c| c.operand.as_str()).collect();
    assert_eq!(operands, vec!["dest.ip"]);
}

/// The binding warning reads the draft, not the policy verdict: a refused
/// duration doesn't make a program rule "apply to every program".
#[test]
fn the_program_warning_ignores_unrelated_problems() {
    let mut d = draft(vec![path("/usr/bin/curl")]);
    d.duration = "1.5h".into();
    let result = check(&d, None);
    assert!(!result.problems.is_empty());
    assert!(
        result.warnings.iter().all(|w| !w.contains("every program")),
        "{:?}",
        result.warnings
    );
}

/// Re-review LOW: the pattern warning says what a pattern can take in, and
/// an unanchored pattern warns that it matches longer paths too.
#[test]
fn a_program_path_pattern_says_what_it_matches() {
    let pattern = |value: &str| {
        draft(vec![
            condition("process.path", MatchKind::Pattern, value),
            condition("dest.host", MatchKind::Exact, "example.com"),
        ])
    };
    let warns = |value: &str| check(&pattern(value), None).warnings;
    let anchored = warns("^/usr/bin/curl$");
    assert!(
        anchored
            .iter()
            .any(|w| w.contains("shells and interpreters")),
        "{anchored:?}"
    );
    assert!(
        anchored.iter().all(|w| !w.contains("^ at the start")),
        "{anchored:?}"
    );
    for loose in ["/usr/bin/curl", "^/usr/bin/curl", "/usr/bin/curl$"] {
        let warnings = warns(loose);
        assert!(
            warnings.iter().any(|w| w.contains("^ at the start")),
            "{loose}: {warnings:?}"
        );
    }
}
