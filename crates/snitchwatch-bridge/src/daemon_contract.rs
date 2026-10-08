//! What the bridge and the bazzite-tower opensnitchd fork agreed on beyond
//! stock v1.8.0's wire format (E3, plan
//! `docs/superpowers/plans/2026-10-08-default-applied-events.md`; the
//! daemon side is bazzite-tower PR #89).
//!
//! The fork reports each connection its `DefaultAction` decided as an
//! ordinary `Statistics.events[]` entry whose synthetic rule is named `""`
//! and described by [`DEFAULT_ACTION_MARKER`]. Each such event grows the
//! daemon's `rule_misses`, never `rule_hits`. The translator
//! (`translator::connection::event_to_row`) and the hit counts
//! (`cache::rule_hits`) both recognise it here.

use snitchwatch_proto::protocol::Rule;

/// The description of the fork's synthetic default-action rule.
pub const DEFAULT_ACTION_MARKER: &str = "snitchwatch:default-action";

/// Whether `rule` is the fork's synthetic default-action rule: named `""`
/// **and** described by exactly [`DEFAULT_ACTION_MARKER`]. Stock v1.8.0 never
/// emits one, but it can load a hand-written rule named `""`, whose events
/// are real rule hits, so the name alone is not enough; and a named rule
/// that copied the description is still that rule.
pub fn is_default_action_rule(rule: &Rule) -> bool {
    rule.name.is_empty() && rule.description == DEFAULT_ACTION_MARKER
}

/// Whether `action` is one the contract allows a default-action event to
/// carry: the action the daemon applied, `allow`, `deny` or `reject`.
pub fn is_contract_default_action(action: &str) -> bool {
    matches!(action, "allow" | "deny" | "reject")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_the_agreed_string() {
        assert_eq!(DEFAULT_ACTION_MARKER, "snitchwatch:default-action");
    }

    #[test]
    fn both_halves_are_needed_and_matched_exactly() {
        let marked = Rule {
            description: DEFAULT_ACTION_MARKER.to_string(),
            ..Default::default()
        };
        assert!(is_default_action_rule(&marked));
        let named = Rule {
            name: "copied".to_string(),
            ..marked.clone()
        };
        assert!(!is_default_action_rule(&named));
        for description in [
            "",
            "Snitchwatch:default-action",
            "snitchwatch:default-action ",
        ] {
            let unmarked = Rule {
                description: description.to_string(),
                ..Default::default()
            };
            assert!(!is_default_action_rule(&unmarked), "{description:?}");
        }
    }

    #[test]
    fn the_contract_names_three_actions() {
        for action in ["allow", "deny", "reject"] {
            assert!(is_contract_default_action(action), "{action}");
        }
        for action in ["", "drop", "Allow", "accept"] {
            assert!(!is_contract_default_action(action), "{action}");
        }
    }
}
