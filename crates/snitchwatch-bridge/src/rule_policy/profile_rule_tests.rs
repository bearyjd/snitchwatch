//! The `ProfileRule` profile (issue #46 Part 2): what a profile rule must
//! be before the bridge installs it. Everything the editor refuses, plus
//! the profile's own limits.

use super::profile::*;
use snitchwatch_proto::protocol::{Operator, Rule};

fn leaf(r#type: &str, operand: &str, data: &str, sensitive: bool) -> Operator {
    Operator {
        r#type: r#type.into(),
        operand: operand.into(),
        data: data.into(),
        sensitive,
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

fn curl() -> Operator {
    leaf("simple", "process.path", "/usr/bin/curl", true)
}

fn rule(operator: Operator) -> Rule {
    Rule {
        name: "850-profile:home:0000-r1".into(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        operator: Some(operator),
        ..Default::default()
    }
}

fn reasons(rule: &Rule) -> Vec<String> {
    validate_user_rule(rule, PolicyProfile::ProfileRule)
        .expect_err("a profile rule should be refused")
        .into_iter()
        .map(|p| p.reason)
        .collect()
}

fn refused_for(rule: &Rule, reason: &str) {
    let found = reasons(rule);
    assert!(found.iter().any(|r| r == reason), "{reason}: {found:?}");
}

#[test]
fn a_profile_rule_the_editor_would_write_passes() {
    for operator in [
        curl(),
        list(vec![
            curl(),
            leaf("simple", "dest.host", "example.com", false),
        ]),
        leaf("simple", "dest.host", "example.com", false),
        leaf("regexp", "process.path", "^/home/[^/]+/\\.steam/", false),
        leaf("network", "dest.network", "10.0.0.0/8", false),
    ] {
        for action in ["allow", "deny", "reject"] {
            let r = Rule {
                action: action.into(),
                ..rule(operator.clone())
            };
            assert_eq!(validate_user_rule(&r, PolicyProfile::ProfileRule), Ok(()));
        }
    }
}

#[test]
fn a_profile_rule_lasts_always_and_never_decides_first_or_hides() {
    for duration in ["until restart", "5m", "once"] {
        let r = Rule {
            duration: duration.into(),
            ..rule(curl())
        };
        refused_for(&r, PROFILE_DURATION_REFUSED);
    }
    refused_for(
        &Rule {
            precedence: true,
            ..rule(curl())
        },
        PROFILE_PRECEDENCE_REFUSED,
    );
    refused_for(
        &Rule {
            nolog: true,
            ..rule(curl())
        },
        PROFILE_NOLOG_REFUSED,
    );
}

/// Part 1's findings: an empty host matches every bare-address connection,
/// and a case-folded program path is #50's bug.
#[test]
fn a_profile_rule_names_a_host_and_matches_a_path_case_exactly() {
    refused_for(
        &rule(list(vec![curl(), leaf("simple", "dest.host", "", false)])),
        EDITOR_EMPTY_HOST_REFUSED,
    );
    refused_for(
        &rule(leaf("simple", "process.path", "/usr/bin/curl", false)),
        PROFILE_CASE_REFUSED,
    );
    refused_for(
        &rule(list(vec![
            curl(),
            leaf("simple", "user.name", "alice", false),
        ])),
        PROFILE_USER_NAME_REFUSED,
    );
}

#[test]
fn a_profile_rule_carries_the_profile_prefix_and_nothing_else_may() {
    let named = |name: &str| Rule {
        name: name.into(),
        ..rule(curl())
    };
    refused_for(&named("899-curl"), PROFILE_PREFIX_REQUIRED);
    for profile in [PolicyProfile::Import, PolicyProfile::Editor] {
        let found = validate_user_rule(&named("850-profile:home:0000-r1"), profile)
            .expect_err("a hand-made rule can't take a profile name");
        assert!(
            found.iter().any(|p| p.reason == PROFILE_NAME_REFUSED),
            "{found:?}"
        );
    }
}

/// Everything the editor refuses, a profile refuses too.
#[test]
fn every_editor_refusal_holds_for_a_profile_rule() {
    for operator in [
        leaf("simple", "process.path", "curl", true),
        leaf("simple", "true", "", false),
        leaf("network", "dest.network", "0.0.0.0/0", false),
        leaf("regexp", "process.path", "/", true),
        list(vec![curl(), leaf("simple", "process.hash.md5", "x", false)]),
        leaf("lists", "lists.domains", "/etc", false),
        list(vec![curl(), leaf("simple", "dest.port", "70000", false)]),
    ] {
        let r = rule(operator);
        let editor = validate_user_rule(
            &Rule {
                name: "899-x".into(),
                ..r.clone()
            },
            PolicyProfile::Editor,
        )
        .expect_err("editor");
        let profile = validate_user_rule(&r, PolicyProfile::ProfileRule).expect_err("profile");
        for problem in editor {
            assert!(
                profile.contains(&problem),
                "{problem:?} missing: {profile:?}"
            );
        }
    }
}
