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
//! Profile rules are named `850-profile:<profile>:<seq04>-<rule>`
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

/// The daemon rule name for a profile's rule at position `seq`.
pub fn rule_name(profile_id: &str, rule_id: &str, seq: usize) -> String {
    format!(
        "{PROFILE_RULE_NAME_PREFIX}{}:{seq:04}-{}",
        sanitize_id(profile_id),
        sanitize_id(rule_id)
    )
}

/// The daemon rule for one of `profile_id`'s rules, at position `seq`, or
/// why it can't be installed.
pub fn materialize_rule(
    profile_id: &str,
    rule: &ProfileRule,
    seq: usize,
) -> Result<Rule, Vec<RuleProblem>> {
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
        name: rule_name(profile_id, &rule.id, seq),
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
pub fn materialize_profile(
    profile_id: &str,
    rules: &[ProfileRule],
) -> Vec<(String, Result<Rule, Vec<RuleProblem>>)> {
    rules
        .iter()
        .enumerate()
        .map(|(seq, rule)| (rule.id.clone(), materialize_rule(profile_id, rule, seq)))
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
