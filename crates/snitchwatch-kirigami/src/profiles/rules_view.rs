//! A profile's rules as the Profiles page lists them (issue #46 Part 2):
//! what each matches, its action, and whether the firewall has it, in
//! plain words. Every string is plain text for `Text.PlainText` labels.

use serde::Serialize;
use snitchwatch_bridge::ws_messages::{
    ProfileRuleWire, ENFORCEMENT_NOT_ENFORCED, ENFORCEMENT_PENDING, ENFORCEMENT_RULE_INSTALLED,
};

use super::row_store::ProfileRow;

/// "Rule installed" means the firewall accepted the rule, not that it has
/// blocked anything yet (the blocklists' wording).
pub const INSTALLED: &str = "Rule installed";
pub const NOT_INSTALLED: &str = "Not installed";
pub const WAITING: &str = "Waiting for the firewall";
pub const INACTIVE: &str = "Applies while this profile is active";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRow {
    pub id: String,
    pub action: String,
    /// What the rule matches, e.g. `process.path = /usr/bin/curl AND …`.
    pub conditions: String,
    pub status: String,
    /// Why, for a rule not installed or still waiting; else empty.
    pub reason: String,
    pub installed: bool,
}

pub fn rule_rows(profile: &ProfileRow) -> Vec<RuleRow> {
    profile
        .rules
        .iter()
        .map(|rule| {
            let (status, installed) = status(profile.active, &rule.enforcement);
            RuleRow {
                id: rule.id.clone(),
                action: rule.action.clone(),
                conditions: conditions(rule),
                status: status.to_string(),
                reason: rule.enforcement_reason.clone().unwrap_or_default(),
                installed,
            }
        })
        .collect()
}

fn status(active: bool, enforcement: &str) -> (&'static str, bool) {
    if !active {
        return (INACTIVE, false);
    }
    match enforcement {
        ENFORCEMENT_RULE_INSTALLED => (INSTALLED, true),
        ENFORCEMENT_NOT_ENFORCED => (NOT_INSTALLED, false),
        ENFORCEMENT_PENDING => (WAITING, false),
        // An older bridge, which installed nothing.
        _ => (NOT_INSTALLED, false),
    }
}

fn conditions(rule: &ProfileRuleWire) -> String {
    let operator = rule.operator.clone().unwrap_or_else(
        || serde_json::json!({ "type": "simple", "operand": rule.operand, "data": rule.data }),
    );
    crate::rules::row_store::Rule {
        operator,
        ..Default::default()
    }
    .operator_summary()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(id: &str, enforcement: &str, reason: Option<&str>) -> ProfileRuleWire {
        ProfileRuleWire {
            id: id.into(),
            action: "deny".into(),
            operator: Some(json!({ "type": "list", "operand": "list", "operands": [
                { "type": "simple", "operand": "process.path", "data": "/usr/bin/curl" },
                { "type": "simple", "operand": "dest.host", "data": "<b>ads</b>.example" },
            ] })),
            enforcement: enforcement.into(),
            enforcement_reason: reason.map(str::to_string),
            ..Default::default()
        }
    }

    fn profile(active: bool, rules: Vec<ProfileRuleWire>) -> ProfileRow {
        ProfileRow {
            id: "home".into(),
            name: "Home".into(),
            active,
            rules,
            ..Default::default()
        }
    }

    #[test]
    fn an_active_profiles_rules_say_whether_the_firewall_has_them() {
        let rows = rule_rows(&profile(
            true,
            vec![
                rule("a", ENFORCEMENT_RULE_INSTALLED, None),
                rule("b", ENFORCEMENT_NOT_ENFORCED, Some("Not installed: x")),
                rule("c", ENFORCEMENT_PENDING, None),
                rule("d", "", None),
            ],
        ));
        let statuses: Vec<(&str, bool)> = rows
            .iter()
            .map(|r| (r.status.as_str(), r.installed))
            .collect();
        assert_eq!(
            statuses,
            vec![
                (INSTALLED, true),
                (NOT_INSTALLED, false),
                (WAITING, false),
                (NOT_INSTALLED, false)
            ]
        );
        assert_eq!(rows[1].reason, "Not installed: x");
        assert!(rows[0].conditions.contains("<b>ads</b>.example"));
        assert!(rows[0].conditions.contains("/usr/bin/curl"));
    }

    /// Never "installed" for a profile that isn't active, whatever a stale
    /// status says.
    #[test]
    fn an_inactive_profiles_rules_apply_only_once_it_is_active() {
        let rows = rule_rows(&profile(
            false,
            vec![rule("a", ENFORCEMENT_RULE_INSTALLED, None)],
        ));
        assert_eq!(rows[0].status, INACTIVE);
        assert!(!rows[0].installed);
    }

    #[test]
    fn a_part_1_rule_shows_its_single_condition() {
        let legacy = ProfileRuleWire {
            id: "r1".into(),
            action: "allow".into(),
            operand: "dest.host".into(),
            data: "nas.local".into(),
            ..Default::default()
        };
        let rows = rule_rows(&profile(true, vec![legacy]));
        assert!(rows[0].conditions.contains("nas.local"), "{:?}", rows[0]);
    }
}
