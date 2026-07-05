//! Convert blocklist entries into opensnitchd deny rules.
//!
//! Each entry produces one rule in the `900–999` specificity band so user rules
//! always win. The rule's `description` field carries a JSON tag
//! `{"snitchwatch": {"source": "blocklist", "list_id": "<id>", "entry": "<host>"}}`
//! so the bridge can re-group rules into the Blocklists tab on the next
//! `ListRules` reconciliation.

use serde::{Deserialize, Serialize};

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

/// Deterministic per-entry materialization.
///
/// Filename layout: `900-blocklist:<sanitized_id>:<seq04>-<host>.json`
/// stored in the rule `name` field; the daemon strips the `.json` suffix when
/// loading. The `900-` prefix puts every blocklist rule below the 0–899 user
/// rule band so user rules win on conflict.
pub fn materialize_entry(list_id: &str, host: &str, seq: usize) -> MaterializedRule {
    let safe_id = sanitize_id(list_id);
    let safe_host = host.to_ascii_lowercase();
    let name = format!("900-blocklist:{safe_id}:{seq:04}-{safe_host}");
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
            rule.name.starts_with("900-blocklist:stevenblack:"),
            "name should be in 900-band: {}",
            rule.name
        );
        assert!(rule.name.contains("doubleclick.net") || rule.name.contains("0017"));
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
