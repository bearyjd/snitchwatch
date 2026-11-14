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

/// Why opensnitchd says it is not using its eBPF process monitor (E3,
/// `docs/superpowers/plans/2026-11-09-ebpf-honest-health.md`). Both mean the
/// daemon attributes connections via `/proc` after a startup failure; see
/// [`MonitorFallback::EbpfEventsDidNotStart`] for a live reload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MonitorFallback {
    /// `opensnitch.o` did not load: the daemon stopped eBPF and uses `/proc`.
    EbpfDidNotStart,
    /// `opensnitch-procs.o` (or tracefs) did not load: the daemon starts its
    /// `/proc` monitors and rolls its method back: to `/proc` at startup, but
    /// to the old method (possibly eBPF) on a live config reload.
    EbpfEventsDidNotStart,
}

/// A daemon alert that says the process monitor fell back to `/proc`. Only
/// ever positive evidence: no report means "not reported", never "eBPF".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMonitorReport {
    pub fallback: MonitorFallback,
    /// Seconds between the bridge receiving the alert and this report.
    pub reported_secs_ago: u64,
    /// The daemon's alert text, as stored (truncated).
    pub daemon_text: String,
}
