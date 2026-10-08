//! How one rule takes part in opensnitchd's decision, in words for the
//! rule inspector (issue #102). The daemon checks enabled rules in name
//! order (`FindFirstMatch`, the simulator's semantics): a matching deny,
//! reject or decide-first rule stops the check; a matching allow doesn't,
//! so a later match can still block, and the last matching allow decides
//! only when nothing blocks. "First match wins" is true only of the rules
//! that stop the check.

use snitchwatch_bridge::translator::verdict::strip_display_hazards;

use super::row_store::{Rule, UNRECOGNISED_ACTION};
use super::simulator::{daemon_action, stops_scan};

pub const OFF: &str = "Turned off: it decides nothing.";
pub const DECIDES_FIRST: &str =
    "Decides first: when it matches, the check stops here and this rule \
     decides, unless an earlier deny, reject or decide-first rule matched first.";
pub const BLOCKS: &str =
    "Blocks when it matches, unless an earlier deny, reject or decide-first rule matched first.";
pub const ALLOWS: &str = "Allows only if no deny, reject, blocklist or decide-first rule matches; \
     when several allows match, the last one decides.";
/// An action other than exactly `allow`, `deny` or `reject` (PR #106
/// review, security L5): the daemon doesn't stop at it and blocks what it
/// decides (`acceptOrDeny`).
pub const UNRECOGNISED: &str = "Its action isn't one the firewall recognises: a match doesn't \
     stop the check, and if no later rule matches, the connection is blocked.";

/// Longest unrecognised action shown, in characters.
const MAX_ACTION_CHARS: usize = 32;

impl Rule {
    /// The rule's action as the Rules page shows it: `allow` or `deny`
    /// (`reject` included), or for any other the action as written, as plain
    /// text, with what the daemon does with it (PR #106 review N4).
    pub fn action_label(&self) -> String {
        let normalized = self.normalized_action();
        if normalized != UNRECOGNISED_ACTION {
            return normalized.to_string();
        }
        let plain = strip_display_hazards(&self.action);
        let shown: String = plain.chars().take(MAX_ACTION_CHARS).collect();
        let cut = if plain.chars().count() > MAX_ACTION_CHARS {
            "…"
        } else {
            ""
        };
        format!("\"{shown}{cut}\" (unrecognised: blocks)")
    }
}

/// The simulator's own reading of a rule (`stops_scan`, `daemon_action`),
/// so the inspector and Simulate never disagree.
pub fn how_it_decides(rule: &Rule) -> &'static str {
    if !rule.enabled {
        OFF
    } else if rule.precedence {
        DECIDES_FIRST
    } else if stops_scan(rule) {
        BLOCKS
    } else if daemon_action(rule) == "allow" {
        ALLOWS
    } else {
        UNRECOGNISED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(action: &str, precedence: bool, enabled: bool) -> Rule {
        Rule {
            action: action.into(),
            precedence,
            enabled,
            ..Default::default()
        }
    }

    /// An allow without precedence never "wins first": the packaged fetch
    /// rule must not read as beating later denies.
    #[test]
    fn each_rule_says_how_it_decides() {
        assert_eq!(how_it_decides(&rule("allow", false, true)), ALLOWS);
        assert_eq!(how_it_decides(&rule("deny", false, true)), BLOCKS);
        assert_eq!(how_it_decides(&rule("reject", false, true)), BLOCKS);
        assert_eq!(how_it_decides(&rule("allow", true, true)), DECIDES_FIRST);
        assert_eq!(how_it_decides(&rule("deny", true, true)), DECIDES_FIRST);
        assert_eq!(how_it_decides(&rule("allow", false, false)), OFF);
        assert!(!ALLOWS.contains("first match wins"));
    }

    /// The Rules page shows an unrecognised action as written, as plain
    /// text and capped, with what the daemon does (N4).
    #[test]
    fn an_unrecognised_action_is_shown_as_written_with_a_note() {
        assert_eq!(rule("allow", false, true).action_label(), "allow");
        assert_eq!(rule("reject", false, true).action_label(), "deny");
        assert_eq!(
            rule("Allow", false, true).normalized_action(),
            UNRECOGNISED_ACTION
        );
        assert_eq!(
            rule("Allow", false, true).action_label(),
            "\"Allow\" (unrecognised: blocks)"
        );
        assert_eq!(
            rule("<b>d\u{202e}rop", false, true).action_label(),
            "\"<b>drop\" (unrecognised: blocks)"
        );
        let long = rule(&"x".repeat(40), false, true).action_label();
        assert!(
            long.starts_with(&format!("\"{}…\"", "x".repeat(32))),
            "{long}"
        );
        assert_eq!(
            rule("", false, true).action_label(),
            "\"\" (unrecognised: blocks)"
        );
    }

    /// The daemon compares actions exactly: "Allow" or "drop" neither stops
    /// the check nor allows.
    #[test]
    fn an_action_the_daemon_does_not_recognise_says_so() {
        for action in ["Allow", "DENY", "drop", ""] {
            assert_eq!(
                how_it_decides(&rule(action, false, true)),
                UNRECOGNISED,
                "{action:?}"
            );
        }
    }
}
