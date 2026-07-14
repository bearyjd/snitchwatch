//! Daemon/kernel readiness diagnostics — combines local kernel probing
//! (this module's `local_checks`) with daemon-reachability and
//! firewall-status signals (assembled by `DiagnosticsCtx` in this same
//! module, wired up in Task 3/4) into the `DiagnosticCheck` list the GUI
//! renders.

pub mod kernel_probe;

use crate::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck};
use kernel_probe::KernelProbe;

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

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

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

/// Combines daemon-reachability (watchdog's `last_ping` staleness),
/// opensnitchd-reported firewall status, and local kernel probes into the
/// full four-check `DiagnosticCheck` list the GUI renders.
pub struct DiagnosticsCtx {
    last_ping: Arc<StdMutex<Instant>>,
    firewall_status: Arc<StdMutex<Option<bool>>>,
    probe: Arc<dyn kernel_probe::KernelProbe>,
}

impl DiagnosticsCtx {
    pub fn new(
        last_ping: Arc<StdMutex<Instant>>,
        firewall_status: Arc<StdMutex<Option<bool>>>,
        probe: Arc<dyn kernel_probe::KernelProbe>,
    ) -> Self {
        Self {
            last_ping,
            firewall_status,
            probe,
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
        let last_ping = {
            let guard = self.last_ping.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let daemon_status = if crate::daemon_watchdog::is_daemon_down(
            last_ping,
            Instant::now(),
            crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT,
        ) {
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
        let last_ping = Arc::new(StdMutex::new(Instant::now()));
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(last_ping, firewall_status, probe);

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
        let stale = Instant::now() - (DAEMON_DOWN_TIMEOUT + std::time::Duration::from_secs(1));
        let last_ping = Arc::new(StdMutex::new(stale));
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(last_ping, firewall_status, probe);

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
    fn reset_firewall_status_unknown_clears_stale_known_status() {
        let last_ping = Arc::new(StdMutex::new(Instant::now()));
        let firewall_status = Arc::new(StdMutex::new(Some(true)));
        let probe: Arc<dyn kernel_probe::KernelProbe> =
            Arc::new(kernel_probe::testing::FakeKernelProbe::all_ok());
        let ctx = DiagnosticsCtx::new(last_ping, firewall_status, probe);

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
}
