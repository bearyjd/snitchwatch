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

#[cfg(test)]
mod tests {
    use super::kernel_probe::testing::FakeKernelProbe;
    use super::*;

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
}
