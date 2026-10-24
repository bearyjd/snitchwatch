//! Local kernel-readiness probes for opensnitchd's nftables firewall backend.
//! Pure host inspection, no opensnitchd
//! involvement — this is what lets diagnostics work even when opensnitchd
//! never connects at all.

pub trait KernelProbe: Send + Sync {
    /// Whether the `nft` binary is reachable on `$PATH`.
    fn nft_on_path(&self) -> bool;
    /// Whether `nf_tables` appears in `/proc/modules` (loaded or built-in
    /// modules both show up there).
    fn nf_tables_module_loaded(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RealKernelProbe;

impl KernelProbe for RealKernelProbe {
    fn nft_on_path(&self) -> bool {
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| dir.join("nft").is_file())
    }

    fn nf_tables_module_loaded(&self) -> bool {
        std::fs::read_to_string("/proc/modules")
            .map(|contents| contents.lines().any(|line| line.starts_with("nf_tables ")))
            .unwrap_or(false)
    }
}

#[cfg(test)]
pub mod testing {
    use super::KernelProbe;

    /// Unit-test-visible fake. Mirrors `scanner-core`'s `MockInspector` /
    /// `scanner-privileged`'s `SyntheticFacts` pattern — builder-style,
    /// every field explicit so a test can't accidentally rely on an
    /// unset-but-truthy default.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct FakeKernelProbe {
        pub nft_on_path: bool,
        pub nf_tables_loaded: bool,
    }

    impl FakeKernelProbe {
        pub fn all_ok() -> Self {
            Self {
                nft_on_path: true,
                nf_tables_loaded: true,
            }
        }
    }

    impl KernelProbe for FakeKernelProbe {
        fn nft_on_path(&self) -> bool {
            self.nft_on_path
        }
        fn nf_tables_module_loaded(&self) -> bool {
            self.nf_tables_loaded
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeKernelProbe;
    use super::*;

    #[test]
    fn all_ok_probe_reports_all_true() {
        let probe = FakeKernelProbe::all_ok();
        assert!(probe.nft_on_path());
        assert!(probe.nf_tables_module_loaded());
    }

    #[test]
    fn default_probe_reports_all_false() {
        let probe = FakeKernelProbe::default();
        assert!(!probe.nft_on_path());
        assert!(!probe.nf_tables_module_loaded());
    }
}
