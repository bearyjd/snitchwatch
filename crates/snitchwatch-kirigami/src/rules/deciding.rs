//! How one rule takes part in opensnitchd's decision, in words for the
//! rule inspector (issue #102). The daemon checks enabled rules in name
//! order (`FindFirstMatch`, the simulator's semantics): a matching deny,
//! reject or decide-first rule stops the check; a matching allow doesn't,
//! so a later match can still block, and the last matching allow decides
//! only when nothing blocks. "First match wins" is true only of the rules
//! that stop the check.

use super::row_store::Rule;
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
