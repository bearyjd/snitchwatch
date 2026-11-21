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

use crate::checks::base_tree::{
    is_commit_hex, lookup_path_ok, parse_checksum, parse_ls_entry, CommitEntry,
};
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

    /// Output of one fixed `rpm -Va --nomtime --noscripts --nodeps`: a line
    /// per file failing verification. Drift is data, not an error (see
    /// `command::run`'s exit allowance).
    fn rpm_verify_all(&self) -> Result<String>;

    /// Name of the package owning `path` (an absolute path), or `None` when
    /// no package owns it.
    fn file_owner(&self, path: &str) -> Result<Option<String>>;

    /// Raw `rpm-ostree status --json`.
    fn deployment_status_json(&self) -> Result<String>;

    /// Whether `ostree` is resolvable in the trusted directories (the
    /// base-tree primitive's pre-check; `false` degrades the whole pass).
    fn ostree_available(&self) -> bool;

    /// What the base commit `commit` holds at the absolute `path` (baseline
    /// spec rule 2), or `None` when the commit has no such path. A failed
    /// lookup is an `Err`, never `None`.
    fn commit_entry(&self, commit: &str, path: &str) -> Result<Option<CommitEntry>>;

    /// The OSTree checksum of what is on disk at the absolute `path`, not
    /// following a final symlink. A path that vanished is an `Err`.
    fn path_checksum(&self, path: &str) -> Result<String>;
}

/// Deadline of one `rpm -qf` owner lookup (the check as a whole is bounded by
/// `checks::file_drift::FILE_DRIFT_BUDGET`; a lookup may overshoot it once).
pub(crate) const FILE_OWNER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Deadline of each `ostree` spawn of the base-tree pass (`ls`, `checksum`).
pub(crate) const OSTREE_LOOKUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Refuse a base-tree lookup that could reach `ostree` as anything but a
/// 64-hex commit (when given) and one absolute path: runs before any spawn.
fn lookup_request(commit: Option<&str>, path: &str) -> Result<()> {
    let refuse = |what: &str| ScanError::Io {
        path: path.to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, what.to_string()),
    };
    if commit.is_some_and(|c| !is_commit_hex(c)) {
        return Err(refuse("base-tree lookup needs a 64-hex commit"));
    }
    if !lookup_path_ok(path) {
        return Err(refuse(
            "base-tree lookup needs an absolute path without control characters",
        ));
    }
    Ok(())
}

/// Absence is proven only by the `OstreeAbsent` allowance (marker stdout);
/// any other output, including an empty one, must parse or fail closed.
fn entry_from_output(out: &str, path: &str) -> Result<Option<CommitEntry>> {
    if out == crate::command::OSTREE_ABSENT_MARKER {
        return Ok(None);
    }
    parse_ls_entry(out, path).map(Some)
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
        crate::command::run(program, args).map_err(|source| Self::run_error(program, source))
    }

    fn run_within(program: &str, args: &[&str], deadline: std::time::Duration) -> Result<String> {
        crate::command::run_with_deadline(program, args, deadline)
            .map_err(|source| Self::run_error(program, source))
    }

    fn run_error(program: &str, source: std::io::Error) -> ScanError {
        if source.kind() == std::io::ErrorKind::NotFound {
            ScanError::PrimitiveUnavailable(format!("{program} not installed"))
        } else {
            ScanError::Io {
                path: program.to_string(),
                source,
            }
        }
    }

    fn which(program: &str) -> Option<String> {
        crate::command::trusted_program(program).map(|path| path.to_string_lossy().into_owned())
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

    fn rpm_verify_all(&self) -> Result<String> {
        let budget = crate::checks::file_drift::FILE_DRIFT_BUDGET;
        Self::run_within("rpm", &crate::command::RPM_VERIFY_ARGS, budget)
    }

    fn file_owner(&self, path: &str) -> Result<Option<String>> {
        if !path.starts_with('/') {
            return Err(ScanError::Io {
                path: path.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "owner lookup needs an absolute path",
                ),
            });
        }
        let args = [
            "-qf",
            "--queryformat",
            crate::command::OWNER_QUERYFORMAT,
            path,
        ];
        let out = Self::run_within("rpm", &args, FILE_OWNER_DEADLINE)?;
        let name = out.lines().next().unwrap_or("").trim();
        if name.is_empty() || name.contains("not owned by any package") {
            Ok(None)
        } else {
            Ok(Some(name.to_string()))
        }
    }

    fn deployment_status_json(&self) -> Result<String> {
        Self::run("rpm-ostree", &["status", "--json"])
    }

    fn ostree_available(&self) -> bool {
        Self::which("ostree").is_some()
    }

    fn commit_entry(&self, commit: &str, path: &str) -> Result<Option<CommitEntry>> {
        lookup_request(Some(commit), path)?;
        let mut args: Vec<&str> = crate::command::OSTREE_LS_ARGS.to_vec();
        args.extend([commit, path]);
        let out = Self::run_within("ostree", &args, OSTREE_LOOKUP_DEADLINE)?;
        entry_from_output(&out, path)
    }

    fn path_checksum(&self, path: &str) -> Result<String> {
        lookup_request(None, path)?;
        let mut args: Vec<&str> = crate::command::OSTREE_CHECKSUM_ARGS.to_vec();
        args.push(path);
        parse_checksum(&Self::run_within("ostree", &args, OSTREE_LOOKUP_DEADLINE)?)
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
        rpm_verify: Option<String>,
        owners: HashMap<String, String>,
        owner_errors: HashSet<String>,
        deployment_json: Option<String>,
        ostree_missing: bool,
        entries: HashMap<String, Option<CommitEntry>>,
        entry_errors: HashSet<String>,
        checksums: HashMap<String, String>,
        checksum_errors: HashSet<String>,
        pub lookups: std::cell::Cell<usize>,
        lookup_delay: std::time::Duration,
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

        pub fn with_rpm_verify(mut self, v: impl Into<String>) -> Self {
            self.rpm_verify = Some(v.into());
            self
        }
        pub fn with_owner(mut self, path: impl Into<String>, pkg: impl Into<String>) -> Self {
            self.owners.insert(path.into(), pkg.into());
            self
        }
        /// `file_owner(path)` fails with an I/O error (a hung/failed `rpm -qf`).
        pub fn with_owner_error(mut self, path: impl Into<String>) -> Self {
            self.owner_errors.insert(path.into());
            self
        }
        pub fn with_deployment_json(mut self, v: impl Into<String>) -> Self {
            self.deployment_json = Some(v.into());
            self
        }
        /// Every `commit_entry` takes this long (budget tests).
        pub fn with_lookup_delay(mut self, delay: std::time::Duration) -> Self {
            self.lookup_delay = delay;
            self
        }
        /// `ostree` is not installed (degraded base-tree pass).
        pub fn without_ostree(mut self) -> Self {
            self.ostree_missing = true;
            self
        }
        /// The commit holds `entry` at `path`.
        pub fn with_commit_entry(mut self, path: impl Into<String>, entry: CommitEntry) -> Self {
            self.entries.insert(path.into(), Some(entry));
            self
        }
        /// The commit has no `path`.
        pub fn with_commit_absent(mut self, path: impl Into<String>) -> Self {
            self.entries.insert(path.into(), None);
            self
        }
        /// `commit_entry(path)` fails (e.g. a failed `ostree ls`).
        pub fn with_commit_entry_error(mut self, path: impl Into<String>) -> Self {
            self.entry_errors.insert(path.into());
            self
        }
        pub fn with_path_checksum(
            mut self,
            path: impl Into<String>,
            sum: impl Into<String>,
        ) -> Self {
            self.checksums.insert(path.into(), sum.into());
            self
        }
        /// `path_checksum(path)` fails (e.g. the file vanished).
        pub fn with_path_checksum_error(mut self, path: impl Into<String>) -> Self {
            self.checksum_errors.insert(path.into());
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
        fn rpm_verify_all(&self) -> Result<String> {
            self.rpm_verify
                .clone()
                .ok_or_else(|| Self::unavailable("rpm_verify_all"))
        }
        fn file_owner(&self, path: &str) -> Result<Option<String>> {
            if self.owner_errors.contains(path) {
                return Err(ScanError::Io {
                    path: path.to_string(),
                    source: std::io::Error::other("synthetic rpm -qf failure"),
                });
            }
            Ok(self.owners.get(path).cloned())
        }
        fn deployment_status_json(&self) -> Result<String> {
            self.deployment_json
                .clone()
                .ok_or_else(|| Self::unavailable("deployment_status_json"))
        }
        fn ostree_available(&self) -> bool {
            !self.ostree_missing
        }
        fn commit_entry(&self, _commit: &str, path: &str) -> Result<Option<CommitEntry>> {
            self.lookups.set(self.lookups.get() + 1);
            std::thread::sleep(self.lookup_delay);
            if self.entry_errors.contains(path) {
                return Err(ScanError::Io {
                    path: path.to_string(),
                    source: std::io::Error::other("synthetic ostree ls failure"),
                });
            }
            self.entries
                .get(path)
                .cloned()
                .ok_or_else(|| Self::unavailable("commit_entry"))
        }
        fn path_checksum(&self, path: &str) -> Result<String> {
            self.lookups.set(self.lookups.get() + 1);
            if self.checksum_errors.contains(path) {
                return Err(ScanError::Io {
                    path: path.to_string(),
                    source: std::io::Error::other("synthetic ostree checksum failure"),
                });
            }
            self.checksums
                .get(path)
                .cloned()
                .ok_or_else(|| Self::unavailable("path_checksum"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "d39c44ff4c9cf2ef204e6e025429558ac2772758b62e458a2a7d068fdd793410";

    #[test]
    fn lookups_refuse_relative_option_like_and_control_character_paths_before_spawning() {
        for bad in ["usr/bin/a", "-d", "--help", "", "/usr/a\nb", "/usr/a\u{1b}"] {
            assert!(lookup_request(None, bad).is_err(), "{bad:?}");
            assert!(lookup_request(Some(COMMIT), bad).is_err(), "{bad:?}");
        }
        assert!(lookup_request(None, "/usr/bin/a").is_ok());
        assert!(lookup_request(Some(COMMIT), "/usr/bin/a").is_ok());
    }

    #[test]
    fn only_the_absent_marker_means_absent_and_empty_output_fails_closed() {
        let path = "/usr/bin/a";
        assert!(entry_from_output("", path).is_err());
        let marker = crate::command::OSTREE_ABSENT_MARKER;
        assert_eq!(entry_from_output(marker, path).unwrap(), None);
    }

    #[test]
    fn a_commit_that_is_not_64_hex_is_refused_before_spawning() {
        for bad in ["unknown", "abc", "", "--help", &COMMIT.to_uppercase()] {
            assert!(lookup_request(Some(bad), "/usr/bin/a").is_err(), "{bad}");
        }
    }

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
