//! Turn a profile's rules into the opensnitchd rules the bridge installs
//! while it is active (issue #46 Part 2).
//!
//! [`materialize_rule`] is the one place a profile rule becomes a daemon
//! rule, and it runs the `ProfileRule` policy on exactly what would be
//! sent: the editor's checks plus `always`, no precedence, no nolog, a
//! named host, a case-sensitive exact program path and the profile prefix.
//! `AddProfileRule`, the enforcer and the rule editor's profile mode all
//! call it, so there is no second, weaker path. A rule it refuses is not
//! installed and keeps the policy's plain-text reasons; nothing is folded
//! into something broader.
//!
//! ## Band placement
//!
//! Profile rules are named `850-profile:<profile>:<rule>`
//! ([`crate::rule_name::PROFILE_RULE_NAME_PREFIX`], reserved: only the
//! bridge sends rules under it). The band sorts before the blocklist band
//! (`z00-blocklist:`, and the legacy `900-blocklist:`), but sort order only
//! decides which of several matching *allows* applies: opensnitchd stops at
//! the first matching deny (`vendor:daemon/rule/loader.go` `FindFirstMatch`)
//! and a profile rule never has `precedence` (owner decision, 2026-10-08),
//! so a profile allow never beats a blocklist's or the user's deny.
//!
//! The `description` carries `{"snitchwatch": {"source": "profile",
//! "profile_id": …, "rule_id": …}}`. With the prefix, it is how the bridge
//! tells its own rules from anyone else's ([`made_by_bridge`]).

use snitchwatch_proto::protocol::{Operator, Rule};

use crate::profiles::store::ProfileRule;
use crate::rule_name::{is_reserved_profile_name, PROFILE_RULE_NAME_PREFIX};
use crate::rule_policy::{validate_user_rule, PolicyProfile, RuleProblem};

/// The band prefix, under its Part 1 name.
pub const PROFILE_BAND_PREFIX: &str = PROFILE_RULE_NAME_PREFIX;

/// Why a saved rule whose id isn't a plain token isn't installed.
pub const RULE_ID_UNUSABLE: &str = "this rule's id can't be used in a firewall rule name; \
     remove the rule and add it again";

/// Why the second rule with an id isn't installed.
pub const DUPLICATE_RULE_ID: &str = "another rule in this profile has the same id";

/// Longest profile rule id (`AddProfileRule`).
pub const MAX_RULE_ID_LEN: usize = 64;

/// Whether `id` is usable as a profile rule id: 1 to 64 ASCII letters,
/// digits, `-` or `_`, so it appears unchanged in the rule's name.
pub fn valid_rule_id(id: &str) -> bool {
    (1..=MAX_RULE_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The daemon rule name for a profile's rule: stable, so adding, removing
/// or replacing one rule never renames (and rewrites) the others.
pub fn rule_name(profile_id: &str, rule_id: &str) -> String {
    format!(
        "{PROFILE_RULE_NAME_PREFIX}{}:{}",
        sanitize_id(profile_id),
        sanitize_id(rule_id)
    )
}

/// The daemon rule for one of `profile_id`'s rules, or why it can't be
/// installed.
pub fn materialize_rule(profile_id: &str, rule: &ProfileRule) -> Result<Rule, Vec<RuleProblem>> {
    if !valid_rule_id(&rule.id) {
        // Part 1 took any id; one that isn't a plain token could name the
        // same rule as another.
        return Err(vec![RuleProblem {
            path: "id".into(),
            reason: RULE_ID_UNUSABLE.into(),
        }]);
    }
    let operator = conditions(rule).map_err(|reason| {
        vec![RuleProblem {
            path: "operator".into(),
            reason,
        }]
    })?;
    let description = serde_json::json!({
        "snitchwatch": {
            "source": "profile",
            "profile_id": profile_id,
            "rule_id": rule.id,
        }
    })
    .to_string();
    let materialized = Rule {
        created: 0,
        name: rule_name(profile_id, &rule.id),
        description,
        enabled: true,
        precedence: false,
        nolog: false,
        action: rule.action.clone(),
        duration: "always".into(),
        operator: Some(operator),
    };
    validate_user_rule(&materialized, PolicyProfile::ProfileRule)?;
    Ok(materialized)
}

/// The rule's conditions: the editor's (`operator`, #48's wire shape), or
/// Part 1's single `simple` condition, case-sensitive on a program path
/// (#50; the stored rule had no way to say).
fn conditions(rule: &ProfileRule) -> Result<Operator, String> {
    match &rule.operator {
        Some(wire) => crate::rule_wire::operator_from_wire(wire),
        None => Ok(Operator {
            r#type: "simple".into(),
            operand: rule.operand.clone(),
            data: rule.data.clone(),
            sensitive: rule.operand == "process.path",
            list: Vec::new(),
        }),
    }
}

/// Every rule of a profile with its outcome, in stored order, by rule id.
/// A repeated id would name the same daemon rule twice: only its first rule
/// is installed.
pub fn materialize_profile(
    profile_id: &str,
    rules: &[ProfileRule],
) -> Vec<(String, Result<Rule, Vec<RuleProblem>>)> {
    let mut seen = std::collections::HashSet::new();
    rules
        .iter()
        .map(|rule| {
            let outcome = if seen.insert(rule.id.as_str()) {
                materialize_rule(profile_id, rule)
            } else {
                Err(vec![RuleProblem {
                    path: "id".into(),
                    reason: DUPLICATE_RULE_ID.into(),
                }])
            };
            (rule.id.clone(), outcome)
        })
        .collect()
}

/// Whether the bridge made `rule`: under the profile prefix *and* tagged as
/// a profile rule. Anything else under the prefix is never deleted.
pub fn made_by_bridge(rule: &Rule) -> bool {
    is_reserved_profile_name(&rule.name)
        && serde_json::from_str::<serde_json::Value>(&rule.description)
            .is_ok_and(|v| v["snitchwatch"]["source"] == "profile")
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "materializer_tests.rs"]
mod tests;
