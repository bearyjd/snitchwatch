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
        for problem in &import {
            assert!(edited.contains(problem), "{problem:?} missing for {case:?}");
        }
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

/// Re-review M3: a blank host name matches every connection that has no
/// host name (every bare-address connection); the editor refuses it. An
/// import keeps it (the import plan's documented exception).
#[test]
fn the_editor_refuses_a_blank_host_name() {
    let blank = rule(list(vec![path_leaf(), leaf("simple", "dest.host", "")]));
    assert!(reasons(&blank)
        .iter()
        .any(|x| x == EDITOR_EMPTY_HOST_REFUSED));
    assert_eq!(validate_user_rule(&blank, PolicyProfile::Import), Ok(()));
    let pattern = rule(list(vec![path_leaf(), leaf("regexp", "dest.host", "^$")]));
    assert_eq!(
        editor(&pattern),
        Ok(()),
        "an explicit pattern says what it means"
    );
}

/// Re-review LOW: the editor's builder offers no process ID or environment
/// condition, and the bridge refuses them for the editor too.
#[test]
fn the_editor_refuses_process_id_and_environment_conditions() {
    for operand in ["process.id", "process.env.HOME"] {
        let r = rule(list(vec![path_leaf(), leaf("simple", operand, "42")]));
        assert!(
            reasons(&r).iter().any(|x| x == EDITOR_OPERAND_REFUSED),
            "{operand}"
        );
    }
}

/// Re-review M2: turning a rule on runs the checks that keep it from
/// matching everything, whatever made it.
#[test]
fn turning_on_needs_a_rule_that_narrows_and_a_valid_duration() {
    for (operator, duration) in [
        (leaf("simple", "true", ""), "always"),
        (leaf("network", "dest.network", "0.0.0.0/0"), "always"),
        (leaf("simple", "dest.host", ""), "always"),
        (
            leaf(
                "simple",
                "process.hash.md5",
                "d41d8cd98f00b204e9800998ecf8427e",
            ),
            "always",
        ),
        (path_leaf(), "forever"),
    ] {
        let r = Rule {
            duration: duration.into(),
            ..rule(operator.clone())
        };
        assert!(
            !super::enable_problems(&r).is_empty(),
            "{operator:?} {duration}"
        );
    }
    assert!(super::enable_problems(&rule(path_leaf())).is_empty());
    let stock = rule(list(vec![path_leaf(), leaf("simple", "process.id", "42")]));
    assert!(
        super::enable_problems(&stock).is_empty(),
        "only what matches everything stops a toggle"
    );
}
