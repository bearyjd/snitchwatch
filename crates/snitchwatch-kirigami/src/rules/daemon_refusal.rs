//! A listed rule the connected daemon won't load (`DaemonPolicy`): an enabled
//! rule with a `network` condition on `source.network`, which opensnitchd 1.9
//! and newer skips when it loads its rules. The daemon still lists it, and
//! the bridge still lets it be edited (`rule_policy::PolicyContext`), so the
//! Rules page says why it does nothing. A disabled rule isn't compiled, so it
//! gets no note.

use snitchwatch_bridge::rule_policy::PolicyContext;

use super::row_store::Rule;
use super::simulator::operator::{parse, Kind, Operator};

pub const SOURCE_NETWORK_NOT_LOADED: &str = "The firewall (opensnitchd 1.9 or newer) doesn't \
     load this rule: it can't match a network on source.network. Edit it to use dest.network, \
     or turn it off.";

/// Why the daemon `context` describes doesn't load `rule`, if it doesn't.
pub fn not_loaded_reason(rule: &Rule, context: PolicyContext) -> Option<&'static str> {
    let refused = context.refuse_source_network
        && rule.enabled
        && uses_source_network(&parse(&rule.operator));
    refused.then_some(SOURCE_NETWORK_NOT_LOADED)
}

fn uses_source_network(op: &Operator) -> bool {
    match op {
        Operator::List(members) => members.iter().any(uses_source_network),
        Operator::Leaf(leaf) => leaf.kind() == Kind::Network && leaf.operand() == "source.network",
        Operator::True | Operator::Unsupported { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn rule(enabled: bool, operator: Value) -> Rule {
        serde_json::from_value(json!({
            "name": "899-lan", "enabled": enabled, "action": "allow", "duration": "always",
            "operator": operator,
        }))
        .expect("rule")
    }

    fn network(operand: &str) -> Value {
        json!({ "type": "network", "operand": operand, "data": "10.0.0.0/8" })
    }

    const NEWER: PolicyContext = PolicyContext {
        refuse_source_network: true,
    };

    #[test]
    fn an_enabled_source_network_rule_is_not_loaded_by_a_newer_daemon() {
        let leaf = rule(true, network("source.network"));
        assert_eq!(
            not_loaded_reason(&leaf, NEWER),
            Some(SOURCE_NETWORK_NOT_LOADED)
        );
        let listed = rule(
            true,
            json!({ "type": "list", "operand": "list", "list": [
                { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl" },
                network("source.network"),
            ]}),
        );
        assert_eq!(
            not_loaded_reason(&listed, NEWER),
            Some(SOURCE_NETWORK_NOT_LOADED)
        );
    }

    #[test]
    fn nothing_is_said_without_the_policy_when_disabled_or_for_dest_network() {
        let leaf = rule(true, network("source.network"));
        assert_eq!(not_loaded_reason(&leaf, PolicyContext::default()), None);
        assert_eq!(
            not_loaded_reason(&rule(false, network("source.network")), NEWER),
            None
        );
        assert_eq!(
            not_loaded_reason(&rule(true, network("dest.network")), NEWER),
            None
        );
    }
}
