//! The `Editor` profile (roadmap P2.1): everything the `Import` profile
//! refuses except timed durations, plus the editor's own checks. A rule
//! written by hand can never do what an imported one can't.

use super::profile::*;
use snitchwatch_proto::protocol::{Operator, Rule};

fn leaf(r#type: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: r#type.into(),
        operand: operand.into(),
        data: data.into(),
        ..Default::default()
    }
}

fn list(members: Vec<Operator>) -> Operator {
    Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: members,
        ..Default::default()
    }
}

fn path_leaf() -> Operator {
    leaf("simple", "process.path", "/usr/bin/curl")
}

fn rule(operator: Operator) -> Rule {
    Rule {
        name: "899-written".into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(operator),
        ..Default::default()
    }
}

fn editor(rule: &Rule) -> Result<(), Vec<RuleProblem>> {
    validate_user_rule(rule, PolicyProfile::Editor)
}

fn reasons(rule: &Rule) -> Vec<String> {
    editor(rule)
        .expect_err("the editor profile should refuse this")
        .into_iter()
        .map(|p| p.reason)
        .collect()
}

#[test]
fn timed_durations_follow_the_cache_grammar_within_bounds() {
    for duration in ["always", "until restart", "10s", "5m", "1h30m", "8760h"] {
        let r = Rule {
            duration: duration.into(),
            ..rule(path_leaf())
        };
        assert_eq!(editor(&r), Ok(()), "{duration}");
    }
    for duration in [
        "1.5h", "300ms", "5", "forever", "", "9s", "8761h", "once", "5m ",
    ] {
        let r = Rule {
            duration: duration.into(),
            ..rule(path_leaf())
        };
        assert!(
            reasons(&r).iter().any(|x| x == EDITOR_DURATION_REFUSED),
            "{duration:?}"
        );
    }
}

/// Owner decision E2: an exact program path must be absolute (and a real
/// program file); a pattern is allowed, the sheet warns about it.
#[test]
fn an_exact_program_path_must_be_absolute_but_a_pattern_may_be_anything() {
    for path in [
        "curl",
        "./curl",
        "bin/curl",
        "/proc/self/exe",
        "/usr//bin/curl",
        "/usr/bin/",
    ] {
        let r = rule(leaf("simple", "process.path", path));
        assert!(
            reasons(&r).iter().any(|x| x == RELATIVE_PATH_REFUSED),
            "{path}"
        );
    }
    assert_eq!(editor(&rule(path_leaf())), Ok(()));
    let pattern = rule(list(vec![
        leaf("regexp", "process.path", "^/home/[^/]+/\\.steam/"),
        leaf("simple", "dest.port", "443"),
    ]));
    assert_eq!(editor(&pattern), Ok(()));
}

#[test]
fn protocol_is_a_short_lowercase_token() {
    for protocol in ["tcp", "udp6", "icmp"] {
        let r = rule(list(vec![
            path_leaf(),
            leaf("simple", "protocol", protocol),
        ]));
        assert_eq!(editor(&r), Ok(()), "{protocol}");
    }
    for protocol in ["TCP", "tcp udp", "t-c-p", "abcdefghijklmnopq"] {
        let r = rule(list(vec![
            path_leaf(),
            leaf("simple", "protocol", protocol),
        ]));
        assert!(
            reasons(&r).iter().any(|x| x == PROTOCOL_REFUSED),
            "{protocol}"
        );
    }
}

/// Every import refusal but the duration one is an editor refusal too.
#[test]
fn every_import_refusal_holds_for_the_editor() {
    let named = |name: &str| Rule {
        name: name.into(),
        ..rule(path_leaf())
    };
    let cases: Vec<Rule> = vec![
        rule(leaf("lists", "lists.domains", "/etc")),
        rule(leaf("network", "dest.network", "LAN")),
        rule(list(vec![
            path_leaf(),
            leaf("simple", "process.hash.md5", "x"),
        ])),
        rule(leaf("simple", "true", "")),
        rule(leaf("regexp", "process.path", "/")),
        rule(leaf("network", "dest.network", "0.0.0.0/0")),
        rule(list(vec![path_leaf(), leaf("simple", "process.env.X", "")])),
        rule(list(vec![
            path_leaf(),
            leaf("simple", "dest.port", "70000"),
        ])),
        named("z00-blocklist:ads:domains"),
        named("900-blocklist:ads:domains"),
        named("snitchwatch-default-x"),
        named("000-snitchwatch-x"),
        named("850-profile:home:0000-r1"),
        named("a/b"),
        Rule {
            action: "drop".into(),
            ..rule(path_leaf())
        },
        Rule {
            description: "d".repeat(crate::cache::rules::MAX_RULE_FIELD_BYTES + 1),
            ..rule(path_leaf())
        },
        Rule {
            operator: None,
            ..rule(path_leaf())
        },
    ];
    for case in cases {
        let import = validate_user_rule(&case, PolicyProfile::Import).expect_err("import");
        let edited = editor(&case).expect_err("editor");
        assert_eq!(edited, import, "{case:?}");
    }
}

#[test]
fn editor_reasons_never_echo_the_rule() {
    let r = Rule {
        duration: "MARKER".into(),
        ..rule(list(vec![
            leaf("simple", "process.path", "MARKER"),
            leaf("simple", "protocol", "MARKER"),
        ]))
    };
    for problem in editor(&r).unwrap_err() {
        let text = format!("{} {}", problem.path, problem.reason).to_lowercase();
        assert!(!text.contains("marker"), "{problem:?}");
    }
}

/// The one entry point the bridge and the GUI share: wire → `Rule` → policy.
#[test]
fn wire_rules_are_checked_with_the_profile() {
    let wire = serde_json::json!({
        "name": "899-x", "enabled": true, "action": "deny", "duration": "5m",
        "operator": { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl",
                      "sensitive": true },
    });
    let parsed = super::check_wire_rule(&wire, PolicyProfile::Editor).unwrap();
    assert_eq!(parsed.duration, "5m");
    assert!(super::check_wire_rule(&wire, PolicyProfile::Import).is_err());
    let broken = serde_json::json!({ "name": "899-x" });
    let problems = super::check_wire_rule(&broken, PolicyProfile::Editor).unwrap_err();
    assert_eq!(problems[0].path, "rule");
}
