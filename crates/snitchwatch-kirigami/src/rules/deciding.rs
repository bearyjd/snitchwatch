//! How one rule takes part in opensnitchd's decision, in words for the
//! rule inspector (issue #102). The daemon checks enabled rules in name
//! order (`FindFirstMatch`, the simulator's semantics): a matching deny,
//! reject or decide-first rule stops the check; a matching allow doesn't,
//! so a later match can still block, and the last matching allow decides
//! only when nothing blocks. "First match wins" is true only of the rules
//! that stop the check.

use super::row_store::Rule;

pub const OFF: &str = "Turned off: it decides nothing.";
pub const DECIDES_FIRST: &str =
    "Decides first: when it matches, the check stops here, before any later rule.";
pub const BLOCKS: &str =
    "Blocks when it matches, unless an earlier deny, reject or decide-first rule matched first.";
pub const ALLOWS: &str = "Allows only if no deny, reject, blocklist or decide-first rule matches; \
     when several allows match, the last one decides.";

pub fn how_it_decides(rule: &Rule) -> &'static str {
    if !rule.enabled {
        OFF
    } else if rule.precedence {
        DECIDES_FIRST
    } else if rule.action.eq_ignore_ascii_case("allow") {
        ALLOWS
    } else {
        BLOCKS
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
}
