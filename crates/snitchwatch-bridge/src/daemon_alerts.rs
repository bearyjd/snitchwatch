//! Store of daemon-reported ERROR/WARNING alerts, one per `Alert.What`
//! category, overlaid onto `DiagnosticsCtx::report()` to close the blind
//! spot where a host-side probe can't see daemon-internal failures (issue
//! #6).
//!
//! ## Persistence semantics
//!
//! Alerts persist until explicitly cleared — there is deliberately no
//! clear-on-`subscribe()` behavior. An earlier version of this store
//! cleared itself on every `subscribe()` call, on the theory that a new
//! daemon session should start clean; that turned out to be wrong for two
//! reasons:
//!
//! 1. It erased still-true alerts on a reconnect that didn't actually fix
//!    anything (the daemon just dropped and re-dialed) — a silent
//!    false-negative on the diagnostics page.
//! 2. It raced the daemon's own alert delivery:
//!    `vendor/opensnitch/daemon/ui/client.go:236-243`'s `onStatusChange`
//!    fires `go c.Subscribe()` concurrently with signalling `isConnected`,
//!    which is what unblocks `alertsDispatcher`'s queued-alert flush — the
//!    two goroutines race, so whether the bridge sees `subscribe()` before
//!    or after a queued alert is undefined from here.
//!
//! Instead, the store is cleared explicitly by
//! `ClientMessage::RecheckDiagnostics` (via `DiagnosticsCtx::clear_alerts`,
//! see `snitchwatch-bridge-cli::run`) — a user-driven "re-baseline". A
//! daemon whose problem persists will re-alert on its next restart, so a
//! stale positive here is recoverable by waiting for that; a silently
//! dropped real alert (the old subscribe-clear behavior's failure mode) is
//! not.
//!
//! ## `Alert.What` in practice
//!
//! opensnitchd v1.8.0's alert *senders* almost never tag a specific `What`:
//! `SendWarningAlert`/`SendErrorAlert` (`daemon/ui/alerts.go:64-70`) — used
//! by every issue-#6-relevant call site (`daemon/main.go:176,187,645`,
//! `daemon/ui/config_utils.go:82`) — hardcode `Alert_GENERIC`. The one
//! caller that does tag a specific `What` (`daemon/main.go:307`,
//! `Alert_KERNEL_EVENT`) sends a `Proc` payload, not `Text`, so it isn't
//! representable by this store's text-only model. In practice, then,
//! essentially every real alert this store ever receives is `GENERIC` plus
//! free text, and `DiagnosticsCtx::report()`'s overlay text-classifies that
//! free text to figure out which check it's actually about — see
//! `classify_generic_alert_text` in `diagnostics/mod.rs`. The `What`-keyed
//! structure here is kept as forward-compat for a future daemon version
//! that tags alerts properly, not because it's what v1.8.0 actually sends.
//!
//! INFO alerts are ignored — only ERROR/WARNING carry troubleshooting
//! weight worth surfacing on the diagnostics page.

use snitchwatch_proto::protocol::alert;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Instant;

/// Stored alert text is truncated to this many bytes (on a char boundary,
/// with a trailing "…") to bound the diagnostics page's memory/rendering
/// cost against an oversized payload from a misbehaving daemon.
const MAX_ALERT_TEXT_BYTES: usize = 512;

/// Severity of a stored alert. A narrower type than the raw proto `i32` so
/// callers can't accidentally store an `INFO` alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Error,
    Warning,
}

impl AlertSeverity {
    /// Maps a raw `Alert.type` value to a severity, or `None` for `INFO`
    /// (or any value that doesn't decode to a known `alert::Type`).
    fn from_proto(raw_type: i32) -> Option<Self> {
        match alert::Type::try_from(raw_type).ok()? {
            alert::Type::Error => Some(Self::Error),
            alert::Type::Warning => Some(Self::Warning),
            alert::Type::Info => None,
        }
    }
}

/// One stored alert: enough to overlay onto a `DiagnosticCheck`'s detail
/// text, including how long ago it was reported.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAlert {
    pub severity: AlertSeverity,
    pub text: String,
    pub recorded_at: Instant,
}

/// Map keyed by `Alert.What`, holding the most recent ERROR/WARNING alert
/// per category. A new alert for the same `What` overwrites the previous
/// one — only the daemon's latest word matters for the overlay. See this
/// module's doc comment for when (and why) entries are cleared.
#[derive(Default)]
pub struct DaemonAlertStore {
    by_what: StdMutex<HashMap<alert::What, StoredAlert>>,
}

impl DaemonAlertStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an alert. A no-op for `INFO` (or any alert whose `raw_type`
    /// doesn't decode to a known severity) — only ERROR/WARNING are stored.
    /// `text` is truncated to [`MAX_ALERT_TEXT_BYTES`] if longer.
    pub fn record(&self, what: alert::What, raw_type: i32, text: String) {
        let Some(severity) = AlertSeverity::from_proto(raw_type) else {
            return;
        };
        let text = truncate_alert_text(text);
        let mut guard = self.by_what.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            what,
            StoredAlert {
                severity,
                text,
                recorded_at: Instant::now(),
            },
        );
    }

    /// The most recently stored alert for `what`, if any.
    pub fn get(&self, what: alert::What) -> Option<StoredAlert> {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&what)
            .cloned()
    }

    /// Drops every stored alert. Called by `ClientMessage::RecheckDiagnostics`
    /// (via `DiagnosticsCtx::clear_alerts`) — see this module's doc comment
    /// for why `subscribe()` deliberately does NOT call this.
    pub fn clear(&self) {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// A full copy of everything currently stored, for tests/diagnostics.
    pub fn snapshot(&self) -> HashMap<alert::What, StoredAlert> {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Truncates `text` to at most [`MAX_ALERT_TEXT_BYTES`] bytes on a char
/// boundary, appending "…" if it was actually truncated.
fn truncate_alert_text(text: String) -> String {
    if text.len() <= MAX_ALERT_TEXT_BYTES {
        return text;
    }
    let mut boundary = MAX_ALERT_TEXT_BYTES;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut truncated = text[..boundary].to_string();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_error_alert_and_returns_it() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "eBPF module failed to load".to_string(),
        );

        let stored = store.get(alert::What::ProcMonitor).unwrap();
        assert_eq!(stored.severity, AlertSeverity::Error);
        assert_eq!(stored.text, "eBPF module failed to load");
    }

    #[test]
    fn records_warning_alert_and_returns_it() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::Firewall,
            alert::Type::Warning as i32,
            "nftables backend unavailable".to_string(),
        );

        let stored = store.get(alert::What::Firewall).unwrap();
        assert_eq!(stored.severity, AlertSeverity::Warning);
    }

    #[test]
    fn info_alert_is_ignored() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::Generic,
            alert::Type::Info as i32,
            "just fyi".to_string(),
        );

        assert!(store.get(alert::What::Generic).is_none());
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn later_alert_for_same_what_overwrites_earlier_one() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::ProcMonitor,
            alert::Type::Warning as i32,
            "first".to_string(),
        );
        store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "second".to_string(),
        );

        let stored = store.get(alert::What::ProcMonitor).unwrap();
        assert_eq!(stored.severity, AlertSeverity::Error);
        assert_eq!(stored.text, "second");
    }

    #[test]
    fn different_what_categories_are_stored_independently() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "ebpf failed".to_string(),
        );
        store.record(
            alert::What::Firewall,
            alert::Type::Error as i32,
            "nft failed".to_string(),
        );

        assert_eq!(store.snapshot().len(), 2);
        assert_eq!(
            store.get(alert::What::ProcMonitor).unwrap().text,
            "ebpf failed"
        );
        assert_eq!(store.get(alert::What::Firewall).unwrap().text, "nft failed");
    }

    #[test]
    fn clear_drops_all_stored_alerts() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "ebpf failed".to_string(),
        );
        store.clear();

        assert!(store.get(alert::What::ProcMonitor).is_none());
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn unrecognized_raw_type_is_ignored_not_stored() {
        let store = DaemonAlertStore::new();
        store.record(alert::What::Generic, 99, "unknown severity".to_string());
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn recorded_at_is_set_close_to_call_time() {
        let store = DaemonAlertStore::new();
        let before = Instant::now();
        store.record(
            alert::What::Generic,
            alert::Type::Error as i32,
            "boom".to_string(),
        );
        let stored = store.get(alert::What::Generic).unwrap();
        assert!(stored.recorded_at >= before);
        assert!(stored.recorded_at.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn long_alert_text_is_truncated_with_ellipsis() {
        let store = DaemonAlertStore::new();
        let long_text = "x".repeat(MAX_ALERT_TEXT_BYTES + 100);
        store.record(alert::What::Generic, alert::Type::Error as i32, long_text);

        let stored = store.get(alert::What::Generic).unwrap();
        assert!(stored.text.len() <= MAX_ALERT_TEXT_BYTES + '…'.len_utf8());
        assert!(stored.text.ends_with('…'));
    }

    #[test]
    fn short_alert_text_is_not_truncated() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::Generic,
            alert::Type::Error as i32,
            "short message".to_string(),
        );

        let stored = store.get(alert::What::Generic).unwrap();
        assert_eq!(stored.text, "short message");
    }
}
