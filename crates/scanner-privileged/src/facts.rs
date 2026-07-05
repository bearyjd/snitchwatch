//! `SystemFacts` — the single interface every sub-check reads the host
//! through. This trait is the seam that makes the classification/diffing
//! logic testable without real hardware, `rpm-ostree`, or `chkrootkit`:
//! tests implement it with synthetic strings, production uses [`LiveSystem`]
//! which shells out to the real primitives.
//!
//! Every method returns [`ScanError::PrimitiveUnavailable`] when the
//! underlying command/path is absent, so orchestration can downgrade an
//! individual sub-check to a "skipped" note instead of failing the scan.

use std::path::Path;
use std::process::Command;

use crate::error::{Result, ScanError};

/// All host reads the scanner performs, behind one trait for testability.
///
/// Provenance for loaded modules is expressed via the same three tiers the
/// baseline spec uses for files: base OSTree tree, layered package, or
/// anomalous. `base_tree_modules` + `module_layered_package` feed tiers 1
/// and 2; `module_signed` informs anomaly severity for tier 3.
pub trait SystemFacts {
    /// Contents of `/proc/cmdline` — the actually-booted kernel arguments.
    fn proc_cmdline(&self) -> Result<String>;

    /// Output of `rpm-ostree kargs` — the committed kernel argument set for
    /// the active deployment (the expected-state source for kargs drift).
    fn committed_kargs(&self) -> Result<String>;

    /// Contents of `/proc/modules` (equivalently `lsmod` minus the header).
    fn proc_modules(&self) -> Result<String>;

    /// Module names shipped under the base tree's
    /// `/usr/lib/modules/$(uname -r)/`. Tier-1 provenance source.
    fn base_tree_modules(&self) -> Result<Vec<String>>;

    /// Name of the layered/DKMS package owning `module`, if any (tier 2).
    fn module_layered_package(&self, module: &str) -> Result<Option<String>>;

    /// Whether `module` carries a signature (`modinfo -F sig_key` non-empty).
    fn module_signed(&self, module: &str) -> Result<bool>;

    /// Active kernel lockdown token from `/sys/kernel/security/lockdown`
    /// (`none` / `integrity` / `confidentiality`).
    fn lockdown_state(&self) -> Result<String>;

    /// Secure Boot enabled? Via `mokutil --sb-state` (or EFI var read).
    fn secure_boot_enabled(&self) -> Result<bool>;

    /// Absolute path to `chkrootkit`, or `None` if not installed.
    fn chkrootkit_path(&self) -> Option<String>;

    /// Run `chkrootkit` and return its stdout. Only called when
    /// [`chkrootkit_path`](Self::chkrootkit_path) returned `Some`.
    fn run_chkrootkit(&self, program: &str) -> Result<String>;
}

/// Production [`SystemFacts`]: reads real sysfs/procfs paths and shells out
/// to `rpm-ostree`, `modinfo`, `rpm`, `mokutil`, `chkrootkit`.
///
/// NOTE: this type holds no state and spawns no background work. Each method
/// is a one-shot read/exec. There is deliberately no constructor that starts
/// anything long-lived — the whole binary is a transient pkexec invocation.
#[derive(Debug, Default, Clone, Copy)]
pub struct LiveSystem;

impl LiveSystem {
    pub fn new() -> Self {
        LiveSystem
    }

    fn read_file(path: &str) -> Result<String> {
        std::fs::read_to_string(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ScanError::PrimitiveUnavailable(format!("{path} not present"))
            } else {
                ScanError::Io {
                    path: path.to_string(),
                    source,
                }
            }
        })
    }

    /// Run `program args...`, returning stdout on success. A missing binary
    /// maps to `PrimitiveUnavailable` so the caller can skip cleanly.
    fn run(program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ScanError::PrimitiveUnavailable(format!("{program} not found on PATH"))
                } else {
                    ScanError::Io {
                        path: program.to_string(),
                        source,
                    }
                }
            })?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn which(program: &str) -> Option<String> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(program);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        None
    }

    fn uname_release() -> Result<String> {
        Ok(Self::run("uname", &["-r"])?.trim().to_string())
    }
}

impl SystemFacts for LiveSystem {
    fn proc_cmdline(&self) -> Result<String> {
        Self::read_file("/proc/cmdline")
    }

    fn committed_kargs(&self) -> Result<String> {
        Self::run("rpm-ostree", &["kargs"])
    }

    fn proc_modules(&self) -> Result<String> {
        Self::read_file("/proc/modules")
    }

    fn base_tree_modules(&self) -> Result<Vec<String>> {
        let release = Self::uname_release()?;
        let dir = format!("/usr/lib/modules/{release}");
        let root = Path::new(&dir);
        if !root.exists() {
            return Err(ScanError::PrimitiveUnavailable(format!(
                "{dir} not present"
            )));
        }
        let mut names = Vec::new();
        collect_module_names(root, &mut names)
            .map_err(|source| ScanError::Io { path: dir, source })?;
        Ok(names)
    }

    fn module_layered_package(&self, module: &str) -> Result<Option<String>> {
        // Resolve the module's on-disk file, then ask rpm which package owns
        // it. `modinfo -n` gives the path; `rpm -qf` the owning package.
        let filename = Self::run("modinfo", &["-n", module])?;
        let filename = filename.trim();
        if filename.is_empty() {
            return Ok(None);
        }
        let owner = Self::run("rpm", &["-qf", filename])?;
        let owner = owner.trim();
        if owner.is_empty() || owner.contains("not owned by any package") {
            Ok(None)
        } else {
            Ok(Some(owner.to_string()))
        }
    }

    fn module_signed(&self, module: &str) -> Result<bool> {
        let sig = Self::run("modinfo", &["-F", "sig_key", module])?;
        Ok(!sig.trim().is_empty())
    }

    fn lockdown_state(&self) -> Result<String> {
        let raw = Self::read_file("/sys/kernel/security/lockdown")?;
        Ok(parse_lockdown_active(&raw))
    }

    fn secure_boot_enabled(&self) -> Result<bool> {
        let out = Self::run("mokutil", &["--sb-state"])?;
        Ok(out.to_lowercase().contains("secureboot enabled"))
    }

    fn chkrootkit_path(&self) -> Option<String> {
        Self::which("chkrootkit")
    }

    fn run_chkrootkit(&self, program: &str) -> Result<String> {
        // `-q` = quiet: print only infected/warning lines.
        Self::run(program, &["-q"])
    }
}

/// Parse `/sys/kernel/security/lockdown`'s bracketed-active format,
/// e.g. `[none] integrity confidentiality` -> `none`.
pub fn parse_lockdown_active(raw: &str) -> String {
    for token in raw.split_whitespace() {
        if let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            return inner.to_string();
        }
    }
    raw.trim().to_string()
}

fn collect_module_names(dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_module_names(&path, out)?;
        } else if let Some(name) = module_name_from_file(&path) {
            out.push(name);
        }
    }
    Ok(())
}

/// Extract a canonical module name from a `.ko`/`.ko.xz`/`.ko.zst` filename,
/// normalizing hyphens to underscores (the kernel treats them equivalently
/// and `/proc/modules` always reports underscores).
fn module_name_from_file(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let base = name.split(".ko").next()?;
    if base == name {
        return None; // no `.ko` segment: not a module file
    }
    Some(base.replace('-', "_"))
}

/// In-memory [`SystemFacts`] for tests: every host read is fed a synthetic
/// value. Unset primitives default to `PrimitiveUnavailable` so a test that
/// forgets to provide one fails loudly rather than silently reading the real
/// host. Builder methods return `Self` for ergonomic per-test setup.
#[cfg(test)]
pub mod testing {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Debug, Default, Clone)]
    pub struct SyntheticFacts {
        proc_cmdline: Option<String>,
        committed_kargs: Option<String>,
        proc_modules: Option<String>,
        base_tree_modules: Option<Vec<String>>,
        layered: HashMap<String, String>,
        signed: HashSet<String>,
        lockdown: Option<String>,
        secure_boot: Option<bool>,
        chkrootkit: Option<String>,
        chkrootkit_output: Option<String>,
    }

    impl SyntheticFacts {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_proc_cmdline(mut self, v: impl Into<String>) -> Self {
            self.proc_cmdline = Some(v.into());
            self
        }
        pub fn with_committed_kargs(mut self, v: impl Into<String>) -> Self {
            self.committed_kargs = Some(v.into());
            self
        }
        pub fn with_proc_modules(mut self, v: impl Into<String>) -> Self {
            self.proc_modules = Some(v.into());
            self
        }
        pub fn with_base_tree_modules<I, S>(mut self, mods: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<String>,
        {
            self.base_tree_modules = Some(mods.into_iter().map(Into::into).collect());
            self
        }
        pub fn with_layered_module(
            mut self,
            module: impl Into<String>,
            pkg: impl Into<String>,
        ) -> Self {
            self.layered.insert(module.into(), pkg.into());
            self
        }
        pub fn with_signed_module(mut self, module: impl Into<String>) -> Self {
            self.signed.insert(module.into());
            self
        }
        pub fn with_lockdown(mut self, v: impl Into<String>) -> Self {
            self.lockdown = Some(v.into());
            self
        }
        pub fn with_secure_boot(mut self, enabled: bool) -> Self {
            self.secure_boot = Some(enabled);
            self
        }
        pub fn with_chkrootkit(
            mut self,
            program: impl Into<String>,
            output: impl Into<String>,
        ) -> Self {
            self.chkrootkit = Some(program.into());
            self.chkrootkit_output = Some(output.into());
            self
        }

        fn unavailable(what: &str) -> ScanError {
            ScanError::PrimitiveUnavailable(format!("synthetic: {what} not configured"))
        }
    }

    impl SystemFacts for SyntheticFacts {
        fn proc_cmdline(&self) -> Result<String> {
            self.proc_cmdline
                .clone()
                .ok_or_else(|| Self::unavailable("proc_cmdline"))
        }
        fn committed_kargs(&self) -> Result<String> {
            self.committed_kargs
                .clone()
                .ok_or_else(|| Self::unavailable("committed_kargs"))
        }
        fn proc_modules(&self) -> Result<String> {
            self.proc_modules
                .clone()
                .ok_or_else(|| Self::unavailable("proc_modules"))
        }
        fn base_tree_modules(&self) -> Result<Vec<String>> {
            // Default to an empty base tree (nothing shipped) so unconfigured
            // modules fall through to the layered/anomalous tiers.
            Ok(self.base_tree_modules.clone().unwrap_or_default())
        }
        fn module_layered_package(&self, module: &str) -> Result<Option<String>> {
            Ok(self.layered.get(module).cloned())
        }
        fn module_signed(&self, module: &str) -> Result<bool> {
            Ok(self.signed.contains(module))
        }
        fn lockdown_state(&self) -> Result<String> {
            self.lockdown
                .clone()
                .ok_or_else(|| Self::unavailable("lockdown_state"))
        }
        fn secure_boot_enabled(&self) -> Result<bool> {
            self.secure_boot
                .ok_or_else(|| Self::unavailable("secure_boot"))
        }
        fn chkrootkit_path(&self) -> Option<String> {
            self.chkrootkit.clone()
        }
        fn run_chkrootkit(&self, _program: &str) -> Result<String> {
            self.chkrootkit_output
                .clone()
                .ok_or_else(|| Self::unavailable("chkrootkit_output"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bracketed_lockdown_state() {
        assert_eq!(
            parse_lockdown_active("[none] integrity confidentiality"),
            "none"
        );
        assert_eq!(
            parse_lockdown_active("none [integrity] confidentiality"),
            "integrity"
        );
        assert_eq!(parse_lockdown_active("plainvalue"), "plainvalue");
    }

    #[test]
    fn normalizes_module_filename() {
        assert_eq!(
            module_name_from_file(Path::new("/lib/modules/x/nvidia-drm.ko.xz")).as_deref(),
            Some("nvidia_drm")
        );
        assert_eq!(
            module_name_from_file(Path::new("/lib/modules/x/xfs.ko")).as_deref(),
            Some("xfs")
        );
        assert_eq!(
            module_name_from_file(Path::new("/lib/modules/x/README")),
            None
        );
    }
}
