//! Convert blocklist entries into opensnitchd deny rules.
//!
//! Each entry produces one rule in the **alpha** `z`-band documented by
//! [`crate::translator::specificity`] (`"z00".."z99"`), so every blocklist
//! filename sorts lexicographically *after* every numeric user-rule prefix
//! (`"789".."999"`) and user rules always win — opensnitchd evaluates rules in
//! ascending filename order (`sort.Strings` in `daemon/rule/loader.go`). The
//! rule's `description` field carries a JSON tag
//! `{"snitchwatch": {"source": "blocklist", "list_id": "<id>", "entry": "<host>"}}`
//! so the bridge can re-group rules into the Blocklists tab (and reconcile them
//! band-independently — see [`crate::grpc_client::classify_rule`]) on the next
//! `ListRules` pass.
//!
//! ## Legacy band migration
//!
//! Earlier builds materialized these rules under a hardcoded decimal
//! `"900-blocklist:"` prefix, which falls *inside* the user-rule numeric range
//! and so violated the "user rules always win" invariant. The band moved to
//! `"z00-blocklist:"`; because the rule *name* changes, a deployed daemon can
//! still hold orphaned old-band denies after an upgrade. The
//! [`RuleSink`](crate::blocklists::RuleSink) contract therefore requires
//! "replace" to purge *every* name in [`owned_blocklist_rule_name_prefixes`]
//! (current **and** legacy) before installing the new set, so a refresh cleans
//! up the old-prefix duplicates. Reconciliation by `description` tag
//! ([`crate::grpc_client::classify_rule`]) is band-agnostic and catches these
//! regardless.

use serde::{Deserialize, Serialize};

use crate::translator::specificity::BLOCKLIST_BAND_PREFIX;

/// Plain-data shape that mirrors the subset of `protocol::ui::Rule` we need
/// when materializing into opensnitchd. The bridge converts this to the prost
/// type at the call boundary in `translator::upstream` — we keep this struct
/// transport-agnostic so the materializer is pure and trivially testable.
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

/// Legacy filename band token used by earlier builds. Retained only so the
/// sync path can recognize and purge orphaned old-band rules on refresh — see
/// [`owned_blocklist_rule_name_prefixes`]. Never emitted for new rules.
const LEGACY_BLOCKLIST_BAND: &str = "900-blocklist:";

/// The band token that leads every current blocklist rule filename, e.g.
/// `"z00-blocklist:"`. Built from [`BLOCKLIST_BAND_PREFIX`] (`"z"`) so it stays
/// in lockstep with the documented alpha band in `translator::specificity`; a
/// unit test asserts it sorts after the weakest user prefix (`"999"`).
fn current_blocklist_band() -> String {
    // "z" + "00" (lowest slot in the "z00".."z99" band) + the ":"-delimited tag.
    format!("{BLOCKLIST_BAND_PREFIX}00-blocklist:")
}

/// The current filename prefix that identifies one list's materialized rules,
/// e.g. `"z00-blocklist:ads:"`.
pub fn blocklist_rule_name_prefix(list_id: &str) -> String {
    format!("{}{}:", current_blocklist_band(), sanitize_id(list_id))
}

/// Every filename prefix (current **and** legacy) under which a list's
/// bridge-managed blocklist rules may currently exist on a daemon. A
/// [`RuleSink`](crate::blocklists::RuleSink) implementation must delete every
/// existing rule whose name starts with any of these before installing the
/// fresh set, so a band migration leaves no orphaned old-prefix denies.
pub fn owned_blocklist_rule_name_prefixes(list_id: &str) -> Vec<String> {
    let safe_id = sanitize_id(list_id);
    vec![
        blocklist_rule_name_prefix(list_id),
        format!("{LEGACY_BLOCKLIST_BAND}{safe_id}:"),
    ]
}

/// Deterministic per-entry materialization.
///
/// Filename layout: `z00-blocklist:<sanitized_id>:<seq04>-<host>.json`
/// stored in the rule `name` field; the daemon strips the `.json` suffix when
/// loading. The alpha `z00-` band sorts after every numeric user-rule prefix
/// (`"789".."999"`) so user rules always win on conflict.
pub fn materialize_entry(list_id: &str, host: &str, seq: usize) -> MaterializedRule {
    let safe_host = host.to_ascii_lowercase();
    let name = format!(
        "{}{seq:04}-{safe_host}",
        blocklist_rule_name_prefix(list_id)
    );
    let description = serde_json::json!({
        "snitchwatch": {
            "source": "blocklist",
            "list_id": list_id,
            "entry": host,
        }
    })
    .to_string();
    MaterializedRule {
        name,
        enabled: true,
        action: "deny".to_string(),
        duration: "always".to_string(),
        description,
        operator: Operator {
            kind: "simple".to_string(),
            operand: "dest.host".to_string(),
            data: safe_host,
        },
    }
}

pub fn materialize_batch(list_id: &str, hosts: &[String]) -> Vec<MaterializedRule> {
    hosts
        .iter()
        .enumerate()
        .map(|(seq, host)| materialize_entry(list_id, host, seq))
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

    #[test]
    fn materializes_single_entry_as_deny_rule() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 17);
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "always");
        assert!(rule.enabled);
        assert!(
            rule.name.starts_with("z00-blocklist:stevenblack:"),
            "name should be in z00-band: {}",
            rule.name
        );
        assert!(rule.name.contains("doubleclick.net") || rule.name.contains("0017"));
    }

    #[test]
    fn current_band_tracks_the_documented_alpha_prefix() {
        // The band token must be built from `specificity::BLOCKLIST_BAND_PREFIX`
        // so the two never drift apart.
        assert!(current_blocklist_band().starts_with(BLOCKLIST_BAND_PREFIX));
        assert_eq!(current_blocklist_band(), "z00-blocklist:");
    }

    #[test]
    fn blocklist_band_sorts_after_every_user_prefix() {
        use crate::translator::specificity::{user_rule_prefix, SpecificityInputs};
        // Strongest possible user rule ("789") and weakest ("999") — the band
        // must sort after both so user rules are always evaluated first.
        let strongest = user_rule_prefix(&SpecificityInputs {
            has_process: true,
            has_remote_host_exact: true,
            has_remote_host_glob: false,
            has_port: true,
            has_protocol: true,
        });
        let weakest = user_rule_prefix(&SpecificityInputs {
            has_process: false,
            has_remote_host_exact: false,
            has_remote_host_glob: false,
            has_port: false,
            has_protocol: false,
        });
        assert_eq!(strongest, "789");
        assert_eq!(weakest, "999");
        let blocklist = materialize_entry("ads", "x.example", 0).name;
        assert!(
            blocklist.as_str() > strongest.as_str(),
            "{blocklist} must sort after strongest user {strongest}"
        );
        assert!(
            blocklist.as_str() > weakest.as_str(),
            "{blocklist} must sort after weakest user {weakest}"
        );
    }

    #[test]
    fn owned_prefixes_cover_current_and_legacy_bands() {
        let prefixes = owned_blocklist_rule_name_prefixes("ads");
        assert!(
            prefixes.contains(&"z00-blocklist:ads:".to_string()),
            "must own current band: {prefixes:?}"
        );
        assert!(
            prefixes.contains(&"900-blocklist:ads:".to_string()),
            "must own legacy band for migration cleanup: {prefixes:?}"
        );
    }

    #[test]
    fn rule_operator_targets_dest_host_simple() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        assert_eq!(rule.operator.kind, "simple");
        assert_eq!(rule.operator.operand, "dest.host");
        assert_eq!(rule.operator.data, "doubleclick.net");
    }

    #[test]
    fn rule_description_carries_source_tag_json() {
        let rule = materialize_entry("stevenblack", "doubleclick.net", 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&rule.description).expect("description is JSON");
        assert_eq!(parsed["snitchwatch"]["source"], "blocklist");
        assert_eq!(parsed["snitchwatch"]["list_id"], "stevenblack");
        assert_eq!(parsed["snitchwatch"]["entry"], "doubleclick.net");
    }

    #[test]
    fn names_are_stable_for_same_input() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 42);
        let b = materialize_entry("stevenblack", "doubleclick.net", 42);
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn names_are_distinct_for_different_seq() {
        let a = materialize_entry("stevenblack", "doubleclick.net", 1);
        let b = materialize_entry("stevenblack", "doubleclick.net", 2);
        assert_ne!(a.name, b.name);
    }

    #[test]
    fn batch_materialize_preserves_order_and_seq() {
        let hosts = vec![
            "a.example".to_string(),
            "b.example".to_string(),
            "c.example".to_string(),
        ];
        let rules = materialize_batch("test", &hosts);
        assert_eq!(rules.len(), 3);
        assert!(rules[0].name.contains("0000"));
        assert!(rules[1].name.contains("0001"));
        assert!(rules[2].name.contains("0002"));
    }

    #[test]
    fn list_id_special_chars_sanitized_in_filename() {
        let rule = materialize_entry("steven/black:bad", "x.example", 0);
        assert!(!rule.name.contains('/'), "filename must not contain slash");
        assert!(
            !rule.name.contains(':') || rule.name.matches(':').count() == 2,
            "exactly the two delimiter colons allowed: {}",
            rule.name
        );
    }
}
