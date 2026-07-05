//! Kernel-argument (kargs) drift check.
//!
//! Compares the actually-booted `/proc/cmdline` against the deployment's
//! committed `rpm-ostree kargs` set. Any runtime argument that is neither
//! committed nor on the curated bootloader/kernel-injected allowlist is
//! flagged as drift — the same "checksum-first, curated-dynamic-allowlist,
//! else-anomalous" shape the baseline spec uses for files, applied to boot
//! arguments (privileged spec §2).

use crate::model::{CheckType, Finding};

/// Argument key-prefixes the bootloader / kernel / initrd inject at boot
/// regardless of committed kargs. Present in `/proc/cmdline` but never in
/// `rpm-ostree kargs`, and expected — so they must not be flagged as drift.
///
/// Curated allowlist (privileged spec §2). Kept deliberately small and
/// explicit; anything not here and not committed is drift.
const BOOT_INJECTED_PREFIXES: &[&str] = &[
    "BOOT_IMAGE=",
    "root=",
    "ostree=",
    "initrd=",
    // dracut/initramfs-added values (rd.*), e.g. rd.luks.uuid, rd.md.uuid.
    "rd.",
];

/// Split a kernel command line into whitespace-separated argument tokens.
/// (Kernel cmdline quoting is rare in practice for the argument *keys* we
/// classify on; whitespace splitting matches how `/proc/cmdline` and
/// `rpm-ostree kargs` both present their values.)
fn tokens(raw: &str) -> Vec<&str> {
    raw.split_whitespace().filter(|t| !t.is_empty()).collect()
}

fn is_boot_injected(arg: &str) -> bool {
    BOOT_INJECTED_PREFIXES
        .iter()
        .any(|prefix| arg.starts_with(prefix))
}

/// Diff booted cmdline against committed kargs. Pure over its string inputs
/// so it is fully testable with synthetic `/proc/cmdline` and
/// `rpm-ostree kargs` fixtures.
pub fn diff_kargs(proc_cmdline: &str, committed_kargs: &str) -> Vec<Finding> {
    let committed: std::collections::HashSet<&str> = tokens(committed_kargs).into_iter().collect();

    tokens(proc_cmdline)
        .into_iter()
        .filter(|arg| !committed.contains(arg) && !is_boot_injected(arg))
        .map(|arg| {
            Finding::anomalous(
                CheckType::KargsDrift,
                format!("kargs:{arg}"),
                format!(
                    "runtime kernel arg '{arg}' present in /proc/cmdline but not in \
                     rpm-ostree kargs committed set for the active deployment"
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_drift_when_cmdline_subset_of_committed() {
        let cmdline =
            "BOOT_IMAGE=/vmlinuz root=UUID=abc ostree=/ostree/1 quiet rhgb rd.luks.uuid=xyz";
        let committed = "quiet rhgb";
        assert!(diff_kargs(cmdline, committed).is_empty());
    }

    #[test]
    fn flags_uncommitted_non_injected_arg() {
        let cmdline = "BOOT_IMAGE=/vmlinuz root=UUID=abc quiet mitigations=off";
        let committed = "quiet";
        let findings = diff_kargs(cmdline, committed);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "kargs:mitigations=off");
        assert_eq!(findings[0].check_type, CheckType::KargsDrift);
    }

    #[test]
    fn committed_arg_is_not_drift_even_if_dangerous_looking() {
        // If the admin deliberately committed mitigations=off, it is expected.
        let findings = diff_kargs("quiet mitigations=off", "quiet mitigations=off");
        assert!(findings.is_empty());
    }

    #[test]
    fn multiple_drift_args_each_flagged() {
        let findings = diff_kargs("a=1 b=2 quiet", "quiet");
        let paths: Vec<_> = findings.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"kargs:a=1"));
        assert!(paths.contains(&"kargs:b=2"));
        assert_eq!(findings.len(), 2);
    }
}
