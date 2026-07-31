//! Daemon/kernel readiness diagnostics — combines local kernel probing
//! (this module's `local_checks`) with daemon-reachability and
//! firewall-status signals (assembled by `DiagnosticsCtx` in this same
//! module, wired up in Task 3/4) into the `DiagnosticCheck` list the GUI
//! renders.

pub mod kernel_probe;

use crate::daemon_alerts::DaemonAlertStore;
use crate::daemon_liveness::DaemonLiveness;
use crate::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck};
use kernel_probe::KernelProbe;
use snitchwatch_proto::protocol::alert;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

pub const EBPF_TROUBLESHOOTING: &str = "This kernel doesn't expose BTF \
    (/sys/kernel/btf/vmlinux missing), which opensnitchd's default \
    ProcMonitorMethod: ebpf requires. Either upgrade to a kernel built with \
    CONFIG_DEBUG_INFO_BTF=y, or set ProcMonitorMethod to proc in \
    opensnitchd's config as a fallback (slower, more overhead, but works on \
    any kernel).";

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

        let mut firewall_status = {
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
        if let Some(alert) = self.alert_store.get(alert::What::Firewall) {
            // The daemon's own word wins over the (possibly stale or absent)
            // subscribe()-reported status.
            firewall_status = CheckStatus::Failed {
                detail: format!(
                    "{FIREWALL_NOT_RUNNING_TROUBLESHOOTING} opensnitchd reports: {}",
                    alert.text
                ),
            };
        }

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

        // PROC_MONITOR or KERNEL_EVENT alert present → EbpfSupport is
        // Failed with the daemon's own alert text appended, even when the
        // local BTF probe passes: the daemon's word wins over the host
        // heuristic (issue #6 — see `daemon_alerts` module doc).
        let ebpf_alert = self
            .alert_store
            .get(alert::What::ProcMonitor)
            .or_else(|| self.alert_store.get(alert::What::KernelEvent));
        if let Some(alert) = ebpf_alert {
            if let Some(ebpf) = checks.iter_mut().find(|c| c.kind == CheckKind::EbpfSupport) {
                ebpf.status = CheckStatus::Failed {
                    detail: format!("{EBPF_TROUBLESHOOTING} opensnitchd reports: {}", alert.text),
                };
            }
        }

        checks
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
}
