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
//! `GenericTopic::classify` below (stored per topic, so one topic's
//! alert never evicts another's). The `What`-keyed
//! structure here is kept as forward-compat for a future daemon version
//! that tags alerts properly, not because it's what v1.8.0 actually sends.
//!
//! INFO alerts are ignored — only ERROR/WARNING carry troubleshooting
//! weight worth surfacing on the diagnostics page.

use crate::ws_messages::MonitorFallback;
use snitchwatch_proto::protocol::alert;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Instant;

/// How long before a daemon Subscribe a monitor-fallback alert still counts
/// as that daemon session's. opensnitchd fires `Subscribe` and its queued
/// startup alerts from two goroutines on the same connect
/// (`daemon/ui/client.go:235-247`, `ui/alerts.go:96-108`), so the alert can
/// land just before the Subscribe; it is never re-sent on a later reconnect.
pub const FALLBACK_SUBSCRIBE_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// `GENERIC` alerts again, one per [`GenericTopic`], so a later alert
    /// about something else (the `EBPF-DNS` one usually follows the
    /// process-monitor one) can't evict it.
    generic_by_topic: StdMutex<HashMap<GenericTopic, StoredAlert>>,
}

/// What a `GENERIC` alert's free text is about. v1.8.0 tags none of its
/// issue-#6 alerts (module doc), so the text is all there is. Matches the
/// strings in the v1.8.0 source; see
/// `docs/superpowers/plans/2026-11-09-ebpf-honest-health.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GenericTopic {
    /// eBPF did not start and the daemon fell back to `/proc`
    /// ([`monitor_fallback`] says which part failed).
    MonitorFallback,
    /// Some other process-monitor or eBPF problem.
    ProcessMonitor,
    /// The DNS eBPF module (`daemon/main.go:650-660`): not the process monitor.
    EbpfDns,
    Firewall,
    Unclassified,
}

impl GenericTopic {
    pub fn classify(text: &str) -> Self {
        let lower = text.to_lowercase();
        if lower.starts_with("ebpf-dns:") {
            Self::EbpfDns
        } else if monitor_fallback(text).is_some() {
            Self::MonitorFallback
        } else if lower.contains("process monitor") || lower.contains("ebpf") {
            Self::ProcessMonitor
        } else if lower.contains("queue #")
            || lower.contains("nfqueue")
            || lower.contains("nftables")
            || lower.contains("firewall")
        {
            Self::Firewall
        } else {
            Self::Unclassified
        }
    }
}

/// Which v1.8.0 monitor-fallback alert `text` is, if any:
/// `procmon/ebpf/ebpf.go:134,141` (`opensnitch.o`; `monitor/init.go:112-131`
/// then runs `/proc`), or `procmon/ebpf/events.go:96-98` and `utils.go:35-37`
/// (procs module or tracefs; `init.go:102-108` starts the `/proc` monitors and
/// `init.go:61-66` rolls the method back). Other `EbpfEventsErr` causes carry
/// bare library text and are not recognised.
pub fn monitor_fallback(text: &str) -> Option<MonitorFallback> {
    let lower = text.to_lowercase();
    if lower.contains("[ebpf] error loading opensnitch.o") {
        Some(MonitorFallback::EbpfDidNotStart)
    } else if lower.contains("unable to load ebpf module (opensnitch-procs.o)")
        || lower.contains("unable to access debugfs filesystem, needed for ebpf")
    {
        Some(MonitorFallback::EbpfEventsDidNotStart)
    } else {
        None
    }
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
        let stored = StoredAlert {
            severity,
            text,
            recorded_at: Instant::now(),
        };
        if what == alert::What::Generic {
            self.generic_by_topic
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(GenericTopic::classify(&stored.text), stored.clone());
        }
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(what, stored);
    }

    /// The most recently stored `GENERIC` alert about `topic`, if any.
    pub fn generic(&self, topic: GenericTopic) -> Option<StoredAlert> {
        self.generic_by_topic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&topic)
            .cloned()
    }

    /// The most recently stored alert for `what`, if any.
    pub fn get(&self, what: alert::What) -> Option<StoredAlert> {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&what)
            .cloned()
    }

    /// A new daemon session (Subscribe): drops the monitor-fallback alert
    /// unless it arrived within [`FALLBACK_SUBSCRIBE_GRACE`] of `now`. Other
    /// alerts are kept (module doc: a reconnect fixes nothing for them).
    /// True if it dropped one.
    ///
    /// Known limitation (E3 review round 4): alert age is only a proxy for
    /// daemon-session identity. opensnitchd's `ClientConfig.id` is
    /// `ts.UnixNano()` per Subscribe, not per process
    /// (`daemon/ui/notifications.go:52`), and nothing else in Subscribe
    /// identifies a process, so a transport reconnect of the same daemon and
    /// a replacement daemon look identical here. The grace window is kept
    /// as is; a stable session id would have to come from an upstream field.
    pub fn on_daemon_subscribe(&self, now: Instant) -> bool {
        let mut guard = self
            .generic_by_topic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let stale = guard.get(&GenericTopic::MonitorFallback).is_some_and(|a| {
            now.saturating_duration_since(a.recorded_at) > FALLBACK_SUBSCRIBE_GRACE
        });
        if stale {
            guard.remove(&GenericTopic::MonitorFallback);
        }
        stale
    }

    /// Drops the monitor-fallback alert only if it was recorded at or before
    /// `sampled_at`, so one recorded after that instant (a replacement
    /// daemon's) survives. True if it dropped one.
    pub fn clear_monitor_fallback_recorded_before(&self, sampled_at: Instant) -> bool {
        let mut guard = self
            .generic_by_topic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let older = guard
            .get(&GenericTopic::MonitorFallback)
            .is_some_and(|a| a.recorded_at <= sampled_at);
        if older {
            guard.remove(&GenericTopic::MonitorFallback);
        }
        older
    }

    /// Drops the stored `GENERIC` alert about `topic` only.
    pub fn clear_generic(&self, topic: GenericTopic) {
        self.generic_by_topic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&topic);
    }

    /// Drops every stored alert. Called by `ClientMessage::RecheckDiagnostics`
    /// (via `DiagnosticsCtx::clear_alerts`) — see this module's doc comment
    /// for why `subscribe()` deliberately does NOT call this.
    pub fn clear(&self) {
        self.by_what
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.generic_by_topic
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
pub(crate) mod tests {
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

    /// The v1.8.0 startup alert when `opensnitch.o` does not load
    /// (`procmon/ebpf/ebpf.go:134` wrapping `core/ebpf.go:69-71`), verbatim.
    pub(crate) const MAIN_MODULE_FALLBACK: &str = "[eBPF] Error loading opensnitch.o: \nunable to load eBPF module (opensnitch.o). Your kernel version (7.2.15-ogc1.1.fc45.x86_64) might not be compatible.\nIf this error persists, change process monitor method to 'proc'";
    /// The v1.8.0 startup alert when `opensnitch-procs.o` does not load
    /// (`procmon/ebpf/events.go:96-98`), verbatim.
    pub(crate) const PROCS_MODULE_FALLBACK: &str = "\nunable to load eBPF module (opensnitch-procs.o). Your kernel version (7.2.15-ogc1.1.fc45.x86_64) might not be compatible.\nIf this error persists, change process monitor method to 'proc'";
    /// `daemon/main.go:650-660`: the DNS module, not the process monitor.
    pub(crate) const DNS_MODULE_ALERT: &str =
        "EBPF-DNS: Unable to attach ebpf listener: unable to load eBPF module (opensnitch-dns.o)";

    #[test]
    fn classifies_every_known_generic_text() {
        use crate::ws_messages::MonitorFallback::*;
        let fallback = GenericTopic::MonitorFallback;
        let cases = [
            (MAIN_MODULE_FALLBACK, fallback, Some(EbpfDidNotStart)),
            ("[eBPF] Error loading opensnitch.o (collection): bad map", fallback, Some(EbpfDidNotStart)),
            (PROCS_MODULE_FALLBACK, fallback, Some(EbpfEventsDidNotStart)),
            ("exit status 32\nUnable to access debugfs filesystem, needed for eBPF to work, likely caused by a hardened or customized kernel.", fallback, Some(EbpfEventsDidNotStart)),
            (DNS_MODULE_ALERT, GenericTopic::EbpfDns, None),
            // `daemon/main.go:645` prints the `*monitor.Error` with %v
            // (`EbpfErr` is 3, `monitor/init.go:20-26`).
            ("Unable to set process monitor method via parameter: &{3 [eBPF] Error loading opensnitch.o: \nunable to load eBPF module (opensnitch.o).}", fallback, Some(EbpfDidNotStart)),
            ("Unable to set process monitor method via parameter: exec format error", GenericTopic::ProcessMonitor, None),
            ("Error creating queue #0: busy", GenericTopic::Firewall, None),
            ("ringbuf: not supported", GenericTopic::Unclassified, None),
        ];
        for (text, topic, cause) in cases {
            assert_eq!(GenericTopic::classify(text), topic, "{text}");
            assert_eq!(monitor_fallback(text), cause, "{text}");
        }
    }

    #[test]
    fn a_later_generic_alert_of_another_topic_keeps_the_fallback() {
        let store = DaemonAlertStore::new();
        store.record(
            alert::What::Generic,
            alert::Type::Warning as i32,
            MAIN_MODULE_FALLBACK.to_string(),
        );
        store.record(
            alert::What::Generic,
            alert::Type::Error as i32,
            DNS_MODULE_ALERT.to_string(),
        );

        let fallback = store.generic(GenericTopic::MonitorFallback).unwrap();
        assert_eq!(fallback.text, MAIN_MODULE_FALLBACK);
        assert_eq!(
            store.get(alert::What::Generic).unwrap().text,
            DNS_MODULE_ALERT
        );
        store.clear();
        assert!(store.generic(GenericTopic::MonitorFallback).is_none());
    }
}
