//! Tests for [`super`]: the one function that turns a profile rule into the
//! daemon rule the bridge installs, and refuses what it must not install.

use super::*;
use crate::rule_policy::RuleProblem;
use serde_json::json;

fn legacy(id: &str, action: &str, operand: &str, data: &str) -> ProfileRule {
    ProfileRule {
        id: id.into(),
        action: action.into(),
        operand: operand.into(),
        data: data.into(),
        operator: None,
    }
}

fn edited(id: &str, action: &str, operator: serde_json::Value) -> ProfileRule {
    ProfileRule {
        id: id.into(),
        action: action.into(),
        operand: String::new(),
        data: String::new(),
        operator: Some(operator),
    }
}

fn reasons(result: Result<Rule, Vec<RuleProblem>>) -> Vec<String> {
    result
        .expect_err("refused")
        .into_iter()
        .map(|p| p.reason)
        .collect()
}

#[test]
fn a_rule_is_installed_in_the_profile_band_as_always_without_precedence() {
    let rule = materialize_rule("home", &legacy("r1", "allow", "dest.host", "nas.local"))
        .expect("installable");
    assert_eq!(rule.name, "850-profile:home:r1");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "always");
    assert!(rule.enabled && !rule.precedence && !rule.nolog);
    let tag: serde_json::Value = serde_json::from_str(&rule.description).unwrap();
    assert_eq!(tag["snitchwatch"]["source"], "profile");
    assert_eq!(tag["snitchwatch"]["profile_id"], "home");
    assert_eq!(tag["snitchwatch"]["rule_id"], "r1");
}

/// #50: a Part 1 rule on a program path is installed case-sensitive, never
/// case-folded; a host stays case-insensitive, as host names are.
#[test]
fn a_saved_program_path_is_matched_case_exactly() {
    let path = materialize_rule(
        "home",
        &legacy("r1", "deny", "process.path", "/usr/bin/curl"),
    )
    .unwrap();
    assert!(path.operator.unwrap().sensitive);
    let host = materialize_rule("home", &legacy("r2", "deny", "dest.host", "x.example")).unwrap();
    assert!(!host.operator.unwrap().sensitive);
}

#[test]
fn an_edited_rule_keeps_its_conditions() {
    let operator = json!({ "type": "list", "operand": "list", "operands": [
        { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl", "sensitive": true },
        { "type": "regexp", "operand": "dest.host", "data": "^(.+\\.)?example\\.com$" },
    ] });
    let rule = materialize_rule("home", &edited("r1", "reject", operator)).unwrap();
    assert_eq!(rule.name, "850-profile:home:r1");
    assert_eq!(rule.action, "reject");
    let op = rule.operator.unwrap();
    assert_eq!(op.r#type, "list");
    assert_eq!(op.list.len(), 2);
    assert_eq!(op.list[1].r#type, "regexp");
}

/// Never silently broadened: a stored rule that fails is refused with the
/// policy's plain text, not folded into something else.
#[test]
fn a_rule_the_policy_refuses_is_not_installed() {
    use crate::rule_policy::reasons::*;
    let empty_host = reasons(materialize_rule(
        "home",
        &legacy("r1", "deny", "dest.host", ""),
    ));
    assert!(
        empty_host.iter().any(|r| r == EDITOR_EMPTY_HOST_REFUSED),
        "{empty_host:?}"
    );
    let unknown_action = reasons(materialize_rule(
        "home",
        &legacy("r1", "drop", "dest.host", "x.example"),
    ));
    assert!(
        unknown_action.iter().any(|r| r == ACTION_REFUSED),
        "{unknown_action:?}"
    );
    let everything = reasons(materialize_rule(
        "home",
        &edited(
            "r1",
            "allow",
            json!({ "type": "simple", "operand": "true", "data": "" }),
        ),
    ));
    assert!(
        everything.iter().any(|r| r == MATCHES_EVERYTHING),
        "{everything:?}"
    );
    let user = reasons(materialize_rule(
        "home",
        &legacy("r1", "deny", "user.name", "alice"),
    ));
    assert!(
        user.iter().any(|r| r == PROFILE_USER_NAME_REFUSED),
        "{user:?}"
    );
    let broken = reasons(materialize_rule("home", &edited("r1", "deny", json!("x"))));
    assert!(!broken.is_empty());
}

#[test]
fn rule_ids_are_short_plain_tokens() {
    for id in ["r1", "a-b_C", &"x".repeat(64)] {
        assert!(valid_rule_id(id), "{id}");
    }
    for id in ["", "a/b", "a b", "a:b", &"x".repeat(65), "é"] {
        assert!(!valid_rule_id(id), "{id}");
    }
}

/// Only rules the bridge made are ever deleted on the strength of the
/// cache: the profile prefix *and* the profile tag.
#[test]
fn made_by_bridge_needs_the_prefix_and_the_tag() {
    let ours = materialize_rule("home", &legacy("r1", "deny", "dest.host", "x.example")).unwrap();
    assert!(made_by_bridge(&ours));
    let untagged = Rule {
        description: String::new(),
        ..ours.clone()
    };
    assert!(!made_by_bridge(&untagged));
    let elsewhere = Rule {
        name: "899-x".into(),
        ..ours.clone()
    };
    assert!(!made_by_bridge(&elsewhere));
    let blocklist_tag = Rule {
        description: json!({ "snitchwatch": { "source": "blocklist" } }).to_string(),
        ..ours
    };
    assert!(!made_by_bridge(&blocklist_tag));
}

#[test]
fn profile_band_sorts_before_blocklist_band() {
    let profile_name = rule_name("home", "r1");
    let blocklist_name = crate::blocklists::materializer::list_rule_name(
        &crate::blocklists::list_dir::IdComponent::from_id("ads"),
        crate::blocklists::materializer::ListKind::Domains,
    );
    assert!(profile_name < blocklist_name);
    assert!(profile_name.as_str() < "900-blocklist:ads:0000-x.example");
}

#[test]
fn ids_with_special_chars_are_sanitized_in_the_name() {
    let name = rule_name("ho/me:x", "r1");
    assert!(!name.contains('/'), "{name}");
    assert!(name.starts_with("850-profile:ho_me_x:"), "{name}");
}

#[test]
fn materialize_profile_keeps_order_and_each_outcome() {
    let rules = vec![
        legacy("r1", "deny", "dest.host", "a.example"),
        legacy("r2", "deny", "dest.host", ""),
        legacy("r3", "allow", "dest.host", "c.example"),
    ];
    let out = materialize_profile("home", &rules);
    let ids: Vec<&str> = out.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["r1", "r2", "r3"]);
    assert_eq!(out[0].1.as_ref().unwrap().name, "850-profile:home:r1");
    assert!(out[1].1.is_err());
    assert_eq!(out[2].1.as_ref().unwrap().name, "850-profile:home:r3");
}

/// PR #104 review: a rule's name doesn't depend on its position, so
/// removing or replacing one never renames (rewrites) the others; a Part 1
/// id that isn't a plain token is refused rather than risk a shared name.
#[test]
fn names_are_stable_and_unusable_ids_are_refused() {
    let rules = vec![
        legacy("r1", "deny", "dest.host", "a.example"),
        legacy("r2", "deny", "dest.host", "b.example"),
    ];
    let before = materialize_profile("home", &rules);
    let after = materialize_profile("home", &rules[1..]);
    assert_eq!(
        before[1].1.as_ref().unwrap().name,
        after[0].1.as_ref().unwrap().name
    );
    for id in ["a/b", "a b", ""] {
        let found = reasons(materialize_rule(
            "home",
            &legacy(id, "deny", "dest.host", "x.example"),
        ));
        assert!(
            found.iter().any(|r| r == RULE_ID_UNUSABLE),
            "{id}: {found:?}"
        );
    }
}

/// A repeated rule id would name the same daemon rule twice: only the first
/// is installed (PR #104 re-review).
#[test]
fn a_repeated_rule_id_is_not_installed_twice() {
    let rules = vec![
        legacy("r1", "deny", "dest.host", "a.example"),
        legacy("r1", "allow", "dest.host", "b.example"),
    ];
    let out = materialize_profile("home", &rules);
    assert!(out[0].1.is_ok());
    let found = reasons(out[1].1.clone());
    assert!(found.iter().any(|r| r == DUPLICATE_RULE_ID), "{found:?}");
}
