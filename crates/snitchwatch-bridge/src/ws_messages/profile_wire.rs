//! The profile wire types, re-exported from `ws_messages`.

use serde::{Deserialize, Serialize};

/// One rule override within a profile, as carried over the wire (mirrors
/// `snitchwatch_bridge::profiles::store::ProfileRule`, kept as a distinct
/// type here the same way `BlocklistSummary` is distinct from
/// `blocklists::store::Subscription` — the wire shape is the stable
/// contract, the store shape is free to evolve independently).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRuleWire {
    pub id: String,
    pub action: String,
    /// Part 1's single condition (empty when `operator` is set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operand: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// The rule editor's conditions in #48's wire shape (issue #46 Part 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<serde_json::Value>,
    /// Bridge to GUI, while the rule's profile is active: one of the
    /// `ENFORCEMENT_*` values ("rule_installed" only after the daemon's
    /// correlated OK). Empty for a profile that isn't active, and from an
    /// older bridge.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub enforcement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub network_matchers: Vec<String>,
    pub rules: Vec<ProfileRuleWire>,
    pub active: bool,
}
