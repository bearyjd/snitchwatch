//! The rule editor's profile mode (issue #46 Part 2): a draft becomes a
//! profile's rule instead of a firewall rule. It is checked by the very
//! function the bridge installs profile rules with
//! (`profiles::materializer::materialize_rule`, the `ProfileRule` policy),
//! so there is no second, weaker path: a profile rule always lasts while
//! its profile is active, never decides first and never hides its
//! connections.

use std::hash::{Hash, Hasher};

use snitchwatch_bridge::profiles::materializer::materialize_rule;
use snitchwatch_bridge::profiles::store::ProfileRule;
use snitchwatch_bridge::ws_messages::{ClientMessage, ProfileRuleWire};

use super::editor::{check, plain_problems, EditorCheck, RuleDraft};

const ALLOW_LOSES: &str =
    "Blocking rules and blocklists still win over this allow: a profile can't override them.";

/// The draft as a profile always installs it: lasting while the profile is
/// active, on, not deciding first, logged.
fn as_profile_rule(draft: &RuleDraft) -> RuleDraft {
    RuleDraft {
        name: String::new(),
        enabled: true,
        duration: "always".into(),
        precedence: false,
        nolog: false,
        ..draft.clone()
    }
}

/// The stored profile rule for a draft, with an id from its content: the
/// same rule added twice replaces itself.
pub fn profile_rule(draft: &RuleDraft) -> ProfileRule {
    let draft = as_profile_rule(draft);
    let operator = draft.to_wire()["operator"].clone();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (draft.action.as_str(), operator.to_string()).hash(&mut hasher);
    ProfileRule {
        id: format!("rule-{:08x}", hasher.finish() as u32),
        action: draft.action,
        operand: String::new(),
        data: String::new(),
        operator: Some(operator),
    }
}

/// Check a draft for `profile_id` exactly as the bridge will install it.
pub fn check_profile(draft: &RuleDraft, profile_id: &str) -> EditorCheck {
    let normalized = as_profile_rule(draft);
    let mut result = EditorCheck {
        warnings: check(&normalized, None).warnings,
        ..Default::default()
    };
    if normalized.action == "allow" {
        result.warnings.push(ALLOW_LOSES.into());
    }
    if let Err(problems) = materialize_rule(profile_id, &profile_rule(draft), 0) {
        result.problems = plain_problems(&problems);
    }
    result
}

/// `AddProfileRule` for a draft, answered by a `RuleCommandResult`.
pub fn profile_message(draft: &RuleDraft, profile_id: &str, request_id: String) -> ClientMessage {
    let rule = profile_rule(draft);
    ClientMessage::AddProfileRule {
        profile_id: profile_id.to_string(),
        rule: ProfileRuleWire {
            id: rule.id,
            action: rule.action,
            operator: rule.operator,
            ..Default::default()
        },
        request_id: Some(request_id),
        reply: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::editor::{new_draft, Condition, MatchKind};

    fn condition(operand: &str, kind: MatchKind, value: &str, sensitive: bool) -> Condition {
        Condition {
            operand: operand.into(),
            kind,
            value: value.into(),
            case_sensitive: sensitive,
        }
    }

    fn curl(host: &str) -> RuleDraft {
        RuleDraft {
            conditions: vec![
                condition("process.path", MatchKind::Exact, "/usr/bin/curl", true),
                condition("dest.host", MatchKind::Exact, host, false),
            ],
            ..new_draft()
        }
    }

    #[test]
    fn a_draft_becomes_a_profile_rule_with_an_id_from_its_content() {
        let rule = profile_rule(&curl("a.example"));
        assert_eq!(rule.action, "deny");
        assert_eq!(
            rule.operator,
            Some(curl("a.example").to_wire()["operator"].clone())
        );
        assert!(snitchwatch_bridge::profiles::materializer::valid_rule_id(
            &rule.id
        ));
        assert_eq!(rule.id, profile_rule(&curl("a.example")).id, "stable");
        assert_ne!(rule.id, profile_rule(&curl("b.example")).id);
        let timed = RuleDraft {
            duration: "5m".into(),
            precedence: true,
            ..curl("a.example")
        };
        assert_eq!(
            profile_rule(&timed),
            rule,
            "a profile rule never carries those"
        );
    }

    /// The bridge's own profile checks, not a weaker copy.
    #[test]
    fn a_profile_rule_is_checked_as_the_bridge_installs_it() {
        assert!(check_profile(&curl("a.example"), "home")
            .problems
            .is_empty());
        let blank = check_profile(&curl(""), "home").problems;
        assert!(
            blank.iter().any(|p| p.contains("blank host name")),
            "{blank:?}"
        );
        let folded = RuleDraft {
            conditions: vec![condition(
                "process.path",
                MatchKind::Exact,
                "/usr/bin/curl",
                false,
            )],
            ..new_draft()
        };
        let problems = check_profile(&folded, "home").problems;
        assert!(
            problems.iter().any(|p| p.contains("upper and lower case")),
            "{problems:?}"
        );
        let by_name = RuleDraft {
            conditions: vec![
                condition("process.path", MatchKind::Exact, "/usr/bin/curl", true),
                condition("user.name", MatchKind::Exact, "alice", false),
            ],
            ..new_draft()
        };
        assert!(!check_profile(&by_name, "home").problems.is_empty());
    }

    #[test]
    fn a_profile_allow_says_blocking_rules_still_win() {
        let allow = RuleDraft {
            action: "allow".into(),
            ..curl("a.example")
        };
        let warnings = check_profile(&allow, "home").warnings;
        assert!(
            warnings.iter().any(|w| w.contains("still win")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().all(|w| !w.contains("Lost when")),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_message_names_the_profile_and_asks_for_a_result() {
        match profile_message(&curl("a.example"), "home", "p-1".into()) {
            ClientMessage::AddProfileRule {
                profile_id,
                rule,
                request_id,
                ..
            } => {
                assert_eq!(profile_id, "home");
                assert_eq!(request_id.as_deref(), Some("p-1"));
                assert!(rule.operator.is_some() && rule.operand.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }
}
