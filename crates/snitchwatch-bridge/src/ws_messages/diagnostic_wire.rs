//! The diagnostic-check wire types, re-exported from `ws_messages`.

use serde::{Deserialize, Serialize};

/// Which readiness/connectivity property a diagnostic check covers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    DaemonReachable,
    FirewallRunning,
    EbpfSupport,
    NftablesSupport,
}

/// Result of one diagnostic check. `Unknown` covers "can't assess yet"
/// (e.g. opensnitchd connected but hasn't sent a `ClientConfig` yet) —
/// never reported as `Ok` or `Failed` when the bridge genuinely doesn't
/// know.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Failed { detail: String },
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub kind: CheckKind,
    pub status: CheckStatus,
}
