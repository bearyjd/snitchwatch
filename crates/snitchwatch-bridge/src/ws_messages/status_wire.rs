//! Traffic, connection-status, about and storage wire types, re-exported
//! from `ws_messages`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEvent {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionsStatus {
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfo {
    pub snitchwatch_version: String,
    pub opensnitchd_version: String,
    pub ebpf_commit: String,
}

/// Where the bridge keeps blocklist subscriptions (`SetBlocklists`) or
/// profiles (`SetProfiles`); each store reports its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    /// True only when the data is saved in a state directory and survives a
    /// restart.
    pub persistent: bool,
    /// Why storage is not persistent, when it was configured but unusable,
    /// or, with `unreadable`, why the saved subscriptions couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The saved data couldn't be read, so Snitchwatch leaves the firewall's
    /// rules for it as they are and installs nothing: blocklists (issue
    /// #45) and the recommended rules' choices (prompt-slot D). Its own
    /// state, apart from `persistent`. An unreadable profile store is
    /// reported as not persistent instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
}

/// What the connected daemon refuses that another version accepts
/// (`crate::rule_policy::PolicyContext`), so a GUI can refuse it first.
/// `daemon_version` is the `major.minor` the bridge read (`None`: unknown,
/// v1.8.0's rules apply).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DaemonPolicy {
    pub refuse_source_network: bool,
    #[serde(default)]
    pub daemon_version: Option<String>,
}

impl DaemonPolicy {
    pub fn for_daemon(version: Option<crate::daemon_config::DaemonVersion>) -> Self {
        Self {
            refuse_source_network: crate::rule_policy::PolicyContext::for_daemon(version)
                .refuse_source_network,
            daemon_version: version.map(|v| v.to_string()),
        }
    }
}
