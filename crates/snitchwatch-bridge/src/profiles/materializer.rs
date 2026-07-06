//! Convert an active profile's rule overrides into opensnitchd rules.
//!
//! ## Band placement
//!
//! `translator::specificity` documents an **alpha** band (`"z00".."z99"`) for
//! blocklist rules, so that every blocklist filename sorts after every possible
//! user-rule filename (`"789".."999"`) and user rules always win.
//! [`crate::blocklists::materializer`] now emits that band (`"z00-blocklist:"`).
//!
//! Profile rule overrides must sort *before* (lower prefix, higher precedence
//! than) both blocklist-sourced denies and low-specificity user rules, so this
//! module places them in a `"850-profile:"` band. `'8' < '9' < 'z'`, so the
//! profiles band sorts before the current blocklist band (`"z00-blocklist:"`)
//! **and** before the legacy `"900-blocklist:"` band a not-yet-migrated daemon
//! may still hold — profile overrides win over blocklist denies in either case.
//! The `profile_band_sorts_before_blocklist_band` test asserts this.
//!
//! Filename layout: `850-profile:<sanitized_id>:<seq04>-<rule_id>.json`
//! (`.json` suffix stripped by the daemon on load, matching
//! `blocklists::materializer`'s convention). The rule's `description` field
//! carries a JSON tag `{"snitchwatch": {"source": "profile", "profile_id":
//! "<id>", "rule_id": "<rule_id>"}}` so a future `ListRules` reconciliation
//! can re-group profile-sourced rules, mirroring the blocklist tag shape.

use serde::{Deserialize, Serialize};

use crate::profiles::store::ProfileRule;

/// Band prefix used for profile-materialized rules. Sorts before the current
/// blocklist band (`"z00-blocklist:"`) and the legacy `"900-blocklist:"` band
/// alike (`'8' < '9' < 'z'`), so profile rule overrides always win over
/// blocklist-sourced denies — see the module docs above.
pub const PROFILE_BAND_PREFIX: &str = "850-profile:";

/// Plain-data shape mirroring the subset of `protocol::ui::Rule` needed to
/// materialize a profile rule override, kept transport-agnostic like
/// [`crate::blocklists::materializer::MaterializedRule`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedRule {
    pub name: String,
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub operator: Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operator {
    #[serde(rename = "type")]
    pub kind: String,
    pub operand: String,
    pub data: String,
}

/// Deterministic per-rule materialization for one profile rule override.
pub fn materialize_rule(profile_id: &str, rule: &ProfileRule, seq: usize) -> MaterializedRule {
    let safe_id = sanitize_id(profile_id);
    let name = format!(
        "{PROFILE_BAND_PREFIX}{safe_id}:{seq:04}-{}",
        sanitize_id(&rule.id)
    );
    let action = if rule.action.eq_ignore_ascii_case("allow") {
        "allow"
    } else {
        "deny"
    };
    let description = serde_json::json!({
        "snitchwatch": {
            "source": "profile",
            "profile_id": profile_id,
            "rule_id": rule.id,
        }
    })
    .to_string();
    MaterializedRule {
        name,
        enabled: true,
        action: action.to_string(),
        duration: "always".to_string(),
        description,
        operator: Operator {
            kind: "simple".to_string(),
            operand: rule.operand.clone(),
            data: rule.data.clone(),
        },
    }
}

/// Materialize every rule override owned by a profile, in stored order.
pub fn materialize_profile(profile_id: &str, rules: &[ProfileRule]) -> Vec<MaterializedRule> {
    rules
        .iter()
        .enumerate()
        .map(|(seq, rule)| materialize_rule(profile_id, rule, seq))
        .collect()
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
mod tests {
    use super::*;

    fn rule(id: &str, action: &str) -> ProfileRule {
        ProfileRule {
            id: id.to_string(),
            action: action.to_string(),
            operand: "dest.host".to_string(),
            data: "nas.local".to_string(),
        }
    }

    #[test]
    fn materializes_single_rule_in_profile_band() {
        let m = materialize_rule("home", &rule("r1", "allow"), 0);
        assert!(
            m.name.starts_with("850-profile:home:"),
            "name should be in 850-profile band: {}",
            m.name
        );
        assert_eq!(m.action, "allow");
        assert!(m.enabled);
        assert_eq!(m.duration, "always");
    }

    #[test]
    fn unknown_action_folds_to_deny() {
        let m = materialize_rule("home", &rule("r1", "reject"), 0);
        assert_eq!(m.action, "deny");
    }

    #[test]
    fn description_carries_source_tag_json() {
        let m = materialize_rule("home", &rule("r1", "deny"), 0);
        let parsed: serde_json::Value = serde_json::from_str(&m.description).unwrap();
        assert_eq!(parsed["snitchwatch"]["source"], "profile");
        assert_eq!(parsed["snitchwatch"]["profile_id"], "home");
        assert_eq!(parsed["snitchwatch"]["rule_id"], "r1");
    }

    #[test]
    fn names_are_distinct_for_different_seq() {
        let a = materialize_rule("home", &rule("r1", "deny"), 1);
        let b = materialize_rule("home", &rule("r1", "deny"), 2);
        assert_ne!(a.name, b.name);
    }

    #[test]
    fn profile_band_sorts_before_blocklist_band() {
        // The profiles band must sort (lexicographically, by rule filename)
        // before every blocklist-band filename, so profile overrides always
        // win over blocklist-sourced denies — for both the current "z00-" band
        // and the legacy "900-" band a not-yet-migrated daemon may still hold.
        let profile_name = materialize_rule("home", &rule("r1", "deny"), 0).name;
        let blocklist_name =
            crate::blocklists::materializer::materialize_entry("ads", "x.example", 0).name;
        assert!(
            blocklist_name.starts_with("z00-blocklist:"),
            "blocklist rule should be in the current z00 band: {blocklist_name}"
        );
        assert!(
            profile_name < blocklist_name,
            "{profile_name} should sort before current band {blocklist_name}"
        );
        assert!(
            profile_name.as_str() < "900-blocklist:ads:0000-x.example",
            "{profile_name} should sort before the legacy 900 band too"
        );
    }

    #[test]
    fn ids_with_special_chars_are_sanitized_in_filename() {
        let m = materialize_rule("ho/me:x", &rule("r/1", "deny"), 0);
        assert!(!m.name.contains('/'), "filename must not contain slash");
    }

    #[test]
    fn materialize_profile_preserves_order() {
        let rules = vec![rule("r1", "deny"), rule("r2", "allow")];
        let out = materialize_profile("home", &rules);
        assert_eq!(out.len(), 2);
        assert!(out[0].name.contains("0000"));
        assert!(out[1].name.contains("0001"));
    }
}
