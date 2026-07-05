//! End-to-end integration test: drive a full privileged scan through the
//! public API with a self-contained fake `SystemFacts` (integration tests
//! compile against the crate WITHOUT `#[cfg(test)]`, so the unit-test
//! `SyntheticFacts` double is not visible here — this fake stands in).
//!
//! Exercises the realistic Phase-6 flow the acceptance criteria describe:
//! userspace flags something → privileged scan runs → report reconciles
//! new/still/resolved across two scans, persisted to a real on-disk scans.db.

use std::collections::{HashMap, HashSet};

use scanner_privileged::error::{Result, ScanError};
use scanner_privileged::facts::SystemFacts;
use scanner_privileged::{run_privileged_scan, ScanStore};

#[derive(Default)]
struct FakeFacts {
    cmdline: String,
    committed_kargs: String,
    proc_modules: String,
    base_tree: HashSet<String>,
    layered: HashMap<String, String>,
    signed: HashSet<String>,
    lockdown: String,
    secure_boot: bool,
    chkrootkit: Option<String>,
    chkrootkit_output: String,
}

impl SystemFacts for FakeFacts {
    fn proc_cmdline(&self) -> Result<String> {
        Ok(self.cmdline.clone())
    }
    fn committed_kargs(&self) -> Result<String> {
        Ok(self.committed_kargs.clone())
    }
    fn proc_modules(&self) -> Result<String> {
        Ok(self.proc_modules.clone())
    }
    fn base_tree_modules(&self) -> Result<Vec<String>> {
        Ok(self.base_tree.iter().cloned().collect())
    }
    fn module_layered_package(&self, module: &str) -> Result<Option<String>> {
        Ok(self.layered.get(module).cloned())
    }
    fn module_signed(&self, module: &str) -> Result<bool> {
        Ok(self.signed.contains(module))
    }
    fn lockdown_state(&self) -> Result<String> {
        Ok(self.lockdown.clone())
    }
    fn secure_boot_enabled(&self) -> Result<bool> {
        Ok(self.secure_boot)
    }
    fn chkrootkit_path(&self) -> Option<String> {
        self.chkrootkit.clone()
    }
    fn run_chkrootkit(&self, _program: &str) -> Result<String> {
        Ok(self.chkrootkit_output.clone())
    }
}

fn healthy_facts() -> FakeFacts {
    FakeFacts {
        cmdline: "BOOT_IMAGE=/vmlinuz root=UUID=x ostree=/ostree/1 quiet".to_string(),
        committed_kargs: "quiet".to_string(),
        proc_modules: "xfs 1 0 - Live 0x0\nnvidia 1 0 - Live 0x0\n".to_string(),
        base_tree: ["xfs".to_string()].into_iter().collect(),
        layered: [("nvidia".to_string(), "akmod-nvidia".to_string())]
            .into_iter()
            .collect(),
        lockdown: "integrity".to_string(),
        secure_boot: true,
        ..Default::default()
    }
}

#[test]
fn full_scan_diffs_new_still_and_resolved_across_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("scans.db");
    let store = ScanStore::open(&db).unwrap();

    // Scan 1: a hidden unsigned module is loaded (the thing the userspace
    // tier would have flagged for deeper inspection).
    let mut facts = healthy_facts();
    facts.proc_modules.push_str("rk_hidden 1 0 - Live 0x0\n");
    let report = run_privileged_scan(&facts, &store, "dep-abc").unwrap();
    assert!(report
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "module:rk_hidden"));
    assert_eq!(report.informational.len(), 2); // secure boot + lockdown

    // Scan 2: same anomaly still present → still outstanding, not new.
    let report2 = run_privileged_scan(&facts, &store, "dep-abc").unwrap();
    assert!(report2.reconciled.new.is_empty());
    assert!(report2
        .reconciled
        .still_outstanding
        .iter()
        .any(|f| f.path == "module:rk_hidden"));

    // Scan 3: module gone (rootkit removed) → resolved.
    let clean = healthy_facts();
    let report3 = run_privileged_scan(&clean, &store, "dep-abc").unwrap();
    assert!(report3
        .reconciled
        .resolved
        .iter()
        .any(|f| f.path == "module:rk_hidden"));
}

#[test]
fn secure_boot_transition_flagged_only_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();

    // Scan 1: secure boot on → informational only.
    let r1 = run_privileged_scan(&healthy_facts(), &store, "dep-abc").unwrap();
    assert!(!r1
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "lockdown:secureboot"));

    // Scan 2: secure boot off → weakening transition → new anomaly.
    let mut off = healthy_facts();
    off.secure_boot = false;
    let r2 = run_privileged_scan(&off, &store, "dep-abc").unwrap();
    assert!(r2
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "lockdown:secureboot"));
}

#[test]
fn chkrootkit_absent_is_skipped_not_treated_clean() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let report = run_privileged_scan(&healthy_facts(), &store, "dep-abc").unwrap();
    assert!(report
        .skipped
        .iter()
        .any(|s| s.check == "rootkit_signature" && s.reason.contains("not installed")));
}

#[test]
fn error_type_is_send_sync() {
    // Compile-time guard: ScanError must be Send + Sync so it flows through
    // anyhow in main and any future threaded caller.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ScanError>();
}
