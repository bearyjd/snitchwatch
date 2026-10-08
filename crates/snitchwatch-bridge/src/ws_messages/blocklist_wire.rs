//! The wire types of the blocklist messages (`SetBlocklists`,
//! `SetBlocklistEntries`), re-exported from `ws_messages`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistSummary {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub entry_count: i64,
    /// The download result (`pending` / `ok` / `failed`), not enforcement.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_iso8601: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
    /// One of the `ENFORCEMENT_*` values. Empty from an older bridge, which
    /// enforced nothing.
    #[serde(default)]
    pub enforcement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_reason: Option<String>,
}

/// Most hosts in one `SetBlocklistEntries` page (~260 KiB of JSON at most).
pub const BLOCKLIST_ENTRIES_PAGE_MAX: u32 = 1000;

/// Longest `request_id` of a `RequestBlocklistEntries` the bridge answers: it
/// is GUI-chosen text that is echoed to every GUI.
pub const MAX_REQUEST_ID_LEN: usize = 64;

/// [`BlocklistSummary::enforcement`]: not downloaded or pushed yet.
pub const ENFORCEMENT_PENDING: &str = "pending";
/// [`BlocklistSummary::enforcement`]: the daemon accepted the list's rule,
/// or already held it unchanged. The daemon may still have loaded 0
/// entries, so GUIs say "Rule installed", never "Enforced".
pub const ENFORCEMENT_RULE_INSTALLED: &str = "rule_installed";
/// [`BlocklistSummary::enforcement`]: nothing blocks this list's hosts; see
/// `enforcement_reason`.
pub const ENFORCEMENT_NOT_ENFORCED: &str = "not_enforced";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistEntry {
    pub host: String,
}
