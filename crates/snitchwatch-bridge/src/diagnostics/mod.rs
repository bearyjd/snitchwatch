//! Daemon/kernel readiness diagnostics — combines local kernel probing
//! (this module's `local_checks`) with daemon-reachability and
//! firewall-status signals (assembled by `DiagnosticsCtx` in this same
//! module, wired up in Task 3/4) into the `DiagnosticCheck` list the GUI
//! renders.

pub mod kernel_probe;

use crate::daemon_alerts::{AlertSeverity, DaemonAlertStore, StoredAlert};
use crate::daemon_liveness::DaemonLiveness;
use crate::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck};
use kernel_probe::KernelProbe;
use snitchwatch_proto::protocol::alert;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

pub const EBPF_TROUBLESHOOTING: &str = "This kernel doesn't expose BTF \
    (/sys/kernel/btf/vmlinux missing), which opensnitchd's default \
    ProcMonitorMethod: ebpf requires. Either upgrade to a kernel built with \
    CONFIG_DEBUG_INFO_BTF=y, or set ProcMonitorMethod to proc in \
    opensnitchd's config as a fallback (slower, more overhead, but works on \
    any kernel).";

/// Used instead of [`EBPF_TROUBLESHOOTING`] when a daemon alert maps to
/// `EbpfSupport` but the local probe says BTF *is* present — using the
/// "kernel doesn't expose BTF" copy there would be actively false. This
/// covers exactly the issue #6 scenario the whole overlay exists for:
/// opensnitchd v1.8.0's bundled eBPF module can fail to load on kernel
/// 6.19+ regardless of BTF presence, and the daemon is the only thing that
/// knows that.
pub const EBPF_DAEMON_REPORTED_TROUBLESHOOTING: &str = "BTF is present on \
    this kernel, but opensnitchd still failed to initialize its eBPF \
    process monitor. Set ProcMonitorMethod to proc in opensnitchd's config \
    as a fallback.";

pub const NFTABLES_TROUBLESHOOTING: &str = "The nft firewall backend \
    opensnitchd depends on isn't available on this host. Install the \
    nftables package, and confirm the kernel wasn't built without \
    CONFIG_NF_TABLES.";

/// Runs the two local (opensnitchd-independent) checks: eBPF/BTF support
/// and nftables support. Always returns exactly these two checks, in this
/// order.
pub fn local_checks(probe: &dyn KernelProbe) -> Vec<DiagnosticCheck> {
    let ebpf_status = if probe.btf_vmlinux_exists() {
        CheckStatus::Ok
    } else {
        CheckStatus::Failed {
            detail: EBPF_TROUBLESHOOTING.to_string(),
        }
    };
    let nftables_status = if probe.nft_on_path() && probe.nf_tables_module_loaded() {
        CheckStatus::Ok
    } else {
        CheckStatus::Failed {
            detail: NFTABLES_TROUBLESHOOTING.to_string(),
        }
    };
    vec![
        DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: ebpf_status,
        },
        DiagnosticCheck {
            kind: CheckKind::NftablesSupport,
            status: nftables_status,
        },
    ]
}

pub const DAEMON_UNREACHABLE_TROUBLESHOOTING: &str = "opensnitchd isn't \
    dialing in. Confirm it's installed and running (systemctl status \
    opensnitchd), and that its Server.Address in \
    /etc/opensnitchd/default-config.json matches the bridge's \
    SNITCHWATCH_GRPC_BIND (default 127.0.0.1:50051). Check \
    /var/log/opensnitchd.log for dial errors.";

pub const FIREWALL_NOT_RUNNING_TROUBLESHOOTING: &str = "opensnitchd \
    connected but its firewall backend isn't active. Check \
    /var/log/opensnitchd.log for nftables errors; confirm nftables is \
    enabled and not conflicting with iptables/firewalld rules already on \
    the host.";

/// Combines daemon-reachability (`DaemonLiveness`), opensnitchd-reported
/// firewall status, and local kernel probes into the full four-check
/// `DiagnosticCheck` list the GUI renders.
pub struct DiagnosticsCtx {
    liveness: DaemonLiveness,
    firewall_status: Arc<StdMutex<Option<bool>>>,
    probe: Arc<dyn kernel_probe::KernelProbe>,
    alert_store: Arc<DaemonAlertStore>,
}

impl DiagnosticsCtx {
    pub fn new(
        liveness: DaemonLiveness,
        firewall_status: Arc<StdMutex<Option<bool>>>,
        probe: Arc<dyn kernel_probe::KernelProbe>,
        alert_store: Arc<DaemonAlertStore>,
    ) -> Self {
        Self {
            liveness,
            firewall_status,
            probe,
            alert_store,
        }
    }

    /// Resets the stored firewall status back to `Unknown`. Called when the
    /// watchdog detects the daemon just went down: the last-known firewall
    /// status is opensnitchd-reported and goes stale the moment the daemon
    /// stops talking to us, so keeping it around would make `report()`
    /// self-contradictory (daemon unreachable, but firewall claimed
    /// running).
    pub fn reset_firewall_status_unknown(&self) {
        let mut guard = self
            .firewall_status
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// Drops every stored daemon alert. Called by
    /// `ClientMessage::RecheckDiagnostics` — the user-driven "re-baseline"
    /// that replaces the old (and wrong — see `daemon_alerts`'s module doc)
    /// clear-on-`subscribe()` behavior.
    pub fn clear_alerts(&self) {
        self.alert_store.clear();
    }

    /// Whichever ERROR/WARNING alert is relevant to `EbpfSupport`, if any.
    /// Prefers a `What`-tagged alert (`ProcMonitor`/`KernelEvent` —
    /// forward-compat for a daemon version that tags alerts properly, see
    /// `daemon_alerts`'s module doc) over a `Generic` alert whose text
    /// classifies to eBPF, since v1.8.0 only ever sends the latter.
    fn ebpf_alert(&self) -> Option<StoredAlert> {
        self.alert_store
            .get(alert::What::ProcMonitor)
            .or_else(|| self.alert_store.get(alert::What::KernelEvent))
            .or_else(|| {
                self.alert_store.get(alert::What::Generic).filter(|a| {
                    classify_generic_alert_text(&a.text) == Some(CheckKind::EbpfSupport)
                })
            })
    }

    /// Whichever ERROR/WARNING alert is relevant to `FirewallRunning`, if
    /// any. Same tagged-then-classified-Generic preference as
    /// [`Self::ebpf_alert`].
    fn firewall_alert(&self) -> Option<StoredAlert> {
        self.alert_store.get(alert::What::Firewall).or_else(|| {
            self.alert_store.get(alert::What::Generic).filter(|a| {
                classify_generic_alert_text(&a.text) == Some(CheckKind::FirewallRunning)
            })
        })
    }

    pub fn report(&self) -> Vec<DiagnosticCheck> {
        let daemon_status = if self
            .liveness
            .is_down(Instant::now(), crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT)
        {
            CheckStatus::Failed {
                detail: DAEMON_UNREACHABLE_TROUBLESHOOTING.to_string(),
            }
        } else {
            CheckStatus::Ok
        };

        let firewall_status = {
            let guard = self
                .firewall_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match *guard {
                Some(true) => CheckStatus::Ok,
                Some(false) => CheckStatus::Failed {
                    detail: FIREWALL_NOT_RUNNING_TROUBLESHOOTING.to_string(),
                },
                None => CheckStatus::Unknown,
            }
        };

        let mut checks = vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: daemon_status,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: firewall_status,
            },
        ];
        checks.extend(local_checks(self.probe.as_ref()));

        // Overlay daemon-reported alerts onto the checks they're actually
        // about. Both ERROR and WARNING severities map to
        // `CheckStatus::Failed` here — the four-check page model only has
        // Ok/Failed/Unknown, no severity-aware middle ground, so a
        // WARNING-severity daemon alert about a check still marks it
        // Failed rather than inventing a third status. The severity isn't
        // silently discarded, though: `append_daemon_alert` prefixes it
        // ("error:"/"warning:") onto the appended detail text.
        if let Some(alert) = self.ebpf_alert() {
            if let Some(ebpf) = checks.iter_mut().find(|c| c.kind == CheckKind::EbpfSupport) {
                // Only claim "kernel doesn't expose BTF" when the local
                // probe actually found that — otherwise that copy would be
                // self-contradictory (see `EBPF_DAEMON_REPORTED_TROUBLESHOOTING`'s
                // doc comment).
                let probe_ok = matches!(ebpf.status, CheckStatus::Ok);
                let base = if probe_ok {
                    EBPF_DAEMON_REPORTED_TROUBLESHOOTING
                } else {
                    EBPF_TROUBLESHOOTING
                };
                ebpf.status = CheckStatus::Failed {
                    detail: append_daemon_alert(base, &alert),
                };
            }
        }
        if let Some(alert) = self.firewall_alert() {
            if let Some(firewall) = checks
                .iter_mut()
                .find(|c| c.kind == CheckKind::FirewallRunning)
            {
                firewall.status = CheckStatus::Failed {
                    detail: append_daemon_alert(FIREWALL_NOT_RUNNING_TROUBLESHOOTING, &alert),
                };
            }
        }

        checks
    }
}

/// Classifies a `GENERIC`-`What` alert's free text into the check it's
/// actually about. opensnitchd v1.8.0 never tags PROC_MONITOR/FIREWALL
/// specifically for any issue-#6-relevant alert — see `daemon_alerts`'s
/// module doc — so a real daemon's eBPF and firewall/queue failures both
/// arrive as `Alert_GENERIC` and have to be told apart by message text
/// instead. Matches on substrings seen in the actual v1.8.0 source
/// (`daemon/main.go:176,187,645`, `daemon/ui/config_utils.go:82`); text
/// matching neither pattern is left unmapped (recorded in the store, but
/// not overlaid onto any check).
fn classify_generic_alert_text(text: &str) -> Option<CheckKind> {
    let lower = text.to_lowercase();
    if lower.contains("process monitor") || lower.contains("ebpf") {
        Some(CheckKind::EbpfSupport)
    } else if lower.contains("queue #")
        || lower.contains("nfqueue")
        || lower.contains("nftables")
        || lower.contains("firewall")
    {
        Some(CheckKind::FirewallRunning)
    } else {
        None
    }
}

/// Appends a stored alert's severity, age, and text onto a base
/// troubleshooting string.
fn append_daemon_alert(base: &str, alert: &StoredAlert) -> String {
    let severity = match alert.severity {
        AlertSeverity::Error => "error",
        AlertSeverity::Warning => "warning",
    };
    format!(
        "{base} opensnitchd reports ({severity}, {} ago): {}",
        format_alert_age(alert.recorded_at.elapsed()),
        alert.text
    )
}

/// Coarse (seconds/minutes) age string for a stored alert's detail text —
/// exact durations aren't useful on a troubleshooting page.
fn format_alert_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::kernel_probe::testing::FakeKernelProbe;
    use super::*;
    use crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;

    #[test]
    fn all_ok_probe_yields_two_ok_checks() {
        let checks = local_checks(&FakeKernelProbe::all_ok());
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    #[test]
    fn missing_btf_yields_failed_ebpf_check() {
        let probe = FakeKernelProbe {
            btf: false,
            nft_on_path: true,
            nf_tables_loaded: true,
        };
        let checks = local_checks(&probe);
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        assert!(matches!(ebpf.status, CheckStatus::Failed { .. }));
    }

    #[test]
    fn missing_nft_binary_yields_failed_nftables_check() {
        let probe = FakeKernelProbe {
            btf: true,
            nft_on_path: false,
            nf_tables_loaded: true,
        };
        let checks = local_checks(&probe);
        let nft = checks
            .iter()
            .find(|c| c.kind == CheckKind::NftablesSupport)
            .unwrap();
        assert!(matches!(nft.status, CheckStatus::Failed { .. }));
    }

    #[test]
    fn report_reflects_daemon_reachable_and_firewall_running() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(
            liveness,
            firewall_status,
            probe,
            Arc::new(DaemonAlertStore::new()),
        );

        let checks = ctx.report();
        assert_eq!(checks.len(), 4);
        let daemon = checks
            .iter()
            .find(|c| c.kind == CheckKind::DaemonReachable)
            .unwrap();
        assert_eq!(daemon.status, CheckStatus::Ok);
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall.status, CheckStatus::Ok);
    }

    #[test]
    fn report_reflects_stale_ping_as_daemon_unreachable() {
        let liveness = DaemonLiveness::new_stale_for_test(
            Instant::now(),
            DAEMON_DOWN_TIMEOUT + std::time::Duration::from_secs(1),
        );
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(
            liveness,
            firewall_status,
            probe,
            Arc::new(DaemonAlertStore::new()),
        );

        let checks = ctx.report();
        let daemon = checks
            .iter()
            .find(|c| c.kind == CheckKind::DaemonReachable)
            .unwrap();
        assert!(matches!(daemon.status, CheckStatus::Failed { .. }));
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall.status, CheckStatus::Unknown);
    }

    #[test]
    fn report_reflects_daemon_reachable_when_stream_open_despite_stale_activity() {
        // The idle-daemon shape this whole fix targets: no recent RPC
        // activity, but a Notifications stream is open — still reachable.
        let liveness = DaemonLiveness::new_stale_for_test(
            Instant::now(),
            DAEMON_DOWN_TIMEOUT + std::time::Duration::from_secs(1),
        );
        liveness.open_notification_stream();
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(
            liveness,
            firewall_status,
            probe,
            Arc::new(DaemonAlertStore::new()),
        );

        let checks = ctx.report();
        let daemon = checks
            .iter()
            .find(|c| c.kind == CheckKind::DaemonReachable)
            .unwrap();
        assert_eq!(daemon.status, CheckStatus::Ok);
    }

    #[test]
    fn reset_firewall_status_unknown_clears_stale_known_status() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(
            liveness,
            firewall_status,
            probe,
            Arc::new(DaemonAlertStore::new()),
        );

        // Sanity: starts out Ok (opensnitchd previously reported it running).
        let before = ctx.report();
        let firewall_before = before
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall_before.status, CheckStatus::Ok);

        ctx.reset_firewall_status_unknown();

        let after = ctx.report();
        let firewall_after = after
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall_after.status, CheckStatus::Unknown);
    }

    #[test]
    fn proc_monitor_alert_fails_ebpf_check_even_when_probe_passes() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        // The probe itself says everything's fine (BTF present) — the
        // daemon's own alert must still win.
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "eBPF module failed to load".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        match &ebpf.status {
            CheckStatus::Failed { detail } => {
                assert!(detail.contains("eBPF module failed to load"));
                // Probe passed (BTF present), so the detail must use the
                // self-consistent "daemon reported" copy, not the
                // "kernel doesn't expose BTF" copy — that would be false
                // here.
                assert!(detail.contains("BTF is present on this kernel"));
                assert!(!detail.contains("doesn't expose BTF"));
                // Severity is surfaced, not silently discarded.
                assert!(detail.contains("(error,"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn proc_monitor_alert_uses_btf_missing_copy_when_probe_also_fails() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> = Arc::new(FakeKernelProbe {
            btf: false,
            nft_on_path: true,
            nf_tables_loaded: true,
        });
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::ProcMonitor,
            alert::Type::Warning as i32,
            "eBPF module failed to load".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        match &ebpf.status {
            CheckStatus::Failed { detail } => {
                // Both the probe and the daemon agree BTF is the problem —
                // the standard BTF-missing copy is accurate here.
                assert!(detail.contains("doesn't expose BTF"));
                assert!(detail.contains("eBPF module failed to load"));
                assert!(detail.contains("(warning,"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn kernel_event_alert_also_fails_ebpf_check() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::KernelEvent,
            alert::Type::Warning as i32,
            "kernel event stream degraded".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        assert!(matches!(ebpf.status, CheckStatus::Failed { .. }));
    }

    #[test]
    fn firewall_alert_fails_firewall_check_even_when_subscribe_reported_running() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::Firewall,
            alert::Type::Error as i32,
            "nftables backend unavailable".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        match &firewall.status {
            CheckStatus::Failed { detail } => {
                assert!(detail.contains("nftables backend unavailable"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn unrelated_alert_what_does_not_affect_any_check() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::Rule,
            alert::Type::Error as i32,
            "a rule matched".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    // The tests below exercise the real shape opensnitchd v1.8.0 actually
    // sends: `Alert_GENERIC` + free text (see `daemon_alerts`'s module doc
    // for why `SendWarningAlert`/`SendErrorAlert` never tag a specific
    // `What`). The strings are taken verbatim from the vendored source.

    #[test]
    fn generic_alert_with_real_v180_process_monitor_text_fails_ebpf_check() {
        // vendor/opensnitch/daemon/main.go:645
        const TEXT: &str = "Unable to set process monitor method via parameter: exec format error";

        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::Generic,
            alert::Type::Warning as i32,
            TEXT.to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        match &ebpf.status {
            CheckStatus::Failed { detail } => assert!(detail.contains(TEXT)),
            other => panic!("expected Failed, got {other:?}"),
        }
        // Must not also spuriously fail the unrelated firewall check.
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        assert_eq!(firewall.status, CheckStatus::Ok);
    }

    #[test]
    fn generic_alert_with_real_v180_queue_text_fails_firewall_check() {
        // vendor/opensnitch/daemon/main.go:176
        const TEXT: &str = "Error creating queue #0: no such device";

        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::Generic,
            alert::Type::Warning as i32,
            TEXT.to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        let firewall = checks
            .iter()
            .find(|c| c.kind == CheckKind::FirewallRunning)
            .unwrap();
        match &firewall.status {
            CheckStatus::Failed { detail } => assert!(detail.contains(TEXT)),
            other => panic!("expected Failed, got {other:?}"),
        }
        let ebpf = checks
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        assert_eq!(ebpf.status, CheckStatus::Ok);
    }

    #[test]
    fn generic_alert_with_unrecognized_text_is_unmapped() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::Generic,
            alert::Type::Error as i32,
            "Something totally unrelated happened".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let checks = ctx.report();
        assert!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    #[test]
    fn clear_alerts_removes_the_overlay_effect() {
        let liveness = DaemonLiveness::new();
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let alert_store = Arc::new(DaemonAlertStore::new());
        alert_store.record(
            alert::What::ProcMonitor,
            alert::Type::Error as i32,
            "eBPF module failed to load".to_string(),
        );
        let ctx = DiagnosticsCtx::new(liveness, firewall_status, probe, alert_store);

        let before = ctx.report();
        let ebpf_before = before
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        assert!(matches!(ebpf_before.status, CheckStatus::Failed { .. }));

        ctx.clear_alerts();

        let after = ctx.report();
        let ebpf_after = after
            .iter()
            .find(|c| c.kind == CheckKind::EbpfSupport)
            .unwrap();
        // This sandbox's kernel genuinely has BTF (verified: FakeKernelProbe
        // isn't in play for `local_checks` here — it is; `all_ok()` reports
        // BTF present), so once the alert stops overriding it, the local
        // probe alone yields Ok.
        assert_eq!(ebpf_after.status, CheckStatus::Ok);
    }

    #[test]
    fn classify_generic_alert_text_is_case_insensitive() {
        assert_eq!(
            classify_generic_alert_text("EBPF init failed"),
            Some(CheckKind::EbpfSupport)
        );
        assert_eq!(
            classify_generic_alert_text("NFTables rule load error"),
            Some(CheckKind::FirewallRunning)
        );
        assert_eq!(classify_generic_alert_text("unrelated message"), None);
    }
}
