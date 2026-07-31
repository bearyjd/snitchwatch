//! Bounded store of the most recent daemon-reported ERROR/WARNING alert per
//! `Alert.What` category.
//!
//! Closes the blind spot verified live 2026-07-31 (issue #6): a host-side
//! kernel probe can say eBPF/BTF support looks fine while opensnitchd's own
//! `PostAlert` RPC (`vendor/opensnitch/daemon/main.go:176,187,645`) reports
//! that it actually failed to load its bundled eBPF module. The probe can't
//! see daemon-internal failures — but the daemon tells us directly, so
//! `grpc_server.rs`'s `post_alert` records into this store and
//! `DiagnosticsCtx::report()` (`diagnostics/mod.rs`) overlays it onto the
//! existing checks.
//!
//! INFO alerts are ignored — only ERROR/WARNING carry troubleshooting
//! weight worth surfacing on the diagnostics page.

use snitchwatch_proto::protocol::alert;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

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
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAlert {
    pub severity: AlertSeverity,
    pub text: String,
}

/// Bounded map keyed by `Alert.What`, holding the most recent ERROR/WARNING
/// alert per category. A new alert for the same `What` overwrites the
/// previous one — only the daemon's latest word matters for the overlay.
/// Cleared on `subscribe()`: a new daemon session starts clean, so a stale
/// alert from a prior run of opensnitchd can't linger on the diagnostics
/// page after a restart that fixed it.
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
    pub fn record(&self, what: alert::What, raw_type: i32, text: String) {
        let Some(severity) = AlertSeverity::from_proto(raw_type) else {
            return;
        };
        let mut guard = self.by_what.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(what, StoredAlert { severity, text });
    }

    /// The most recently stored alert for `what`, if any.
    pub fn get(&self, what: alert::What) -> Option<StoredAlert> {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&what)
            .cloned()
    }

    /// Drops every stored alert. Called on `subscribe()`.
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
}
