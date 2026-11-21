//! End-to-end integration test: drive a full privileged scan through the
//! public API with a self-contained fake `SystemFacts` (integration tests
//! compile against the crate WITHOUT `#[cfg(test)]`, so the unit-test
//! `SyntheticFacts` double is not visible here — this fake stands in).
//!
//! Exercises the realistic Phase-6 flow the acceptance criteria describe:
//! userspace flags something → privileged scan runs → report reconciles
//! new/still/resolved across two scans, persisted to a real on-disk scans.db.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use scanner_privileged::checks::base_tree::CommitEntry;
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
    rpm_verify: Option<String>,
    rpm_verify_io_error: bool,
    owners: HashMap<String, String>,
    deployment_json: Option<String>,
    ostree: bool,
    entries: HashMap<String, CommitEntry>,
    disk: HashMap<String, String>,
    status_reads: Cell<usize>,
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
    fn rpm_verify_all(&self) -> Result<String> {
        if self.rpm_verify_io_error {
            return Err(ScanError::Io {
                path: "rpm".to_string(),
                source: std::io::Error::other("rpmdb locked"),
            });
        }
        self.rpm_verify
            .clone()
            .ok_or_else(|| ScanError::PrimitiveUnavailable("fake: no rpm".to_string()))
    }
    fn file_owner(&self, path: &str) -> Result<Option<String>> {
        Ok(self.owners.get(path).cloned())
    }
    fn deployment_status_json(&self) -> Result<String> {
        self.status_reads.set(self.status_reads.get() + 1);
        self.deployment_json
            .clone()
            .ok_or_else(|| ScanError::PrimitiveUnavailable("fake: no rpm-ostree".to_string()))
    }
    fn ostree_available(&self) -> bool {
        self.ostree
    }
    fn commit_entry(&self, _commit: &str, path: &str) -> Result<Option<CommitEntry>> {
        if !self.ostree {
            return Err(ScanError::PrimitiveUnavailable(
                "fake: no base tree".to_string(),
            ));
        }
        Ok(self.entries.get(path).cloned())
    }
    fn path_checksum(&self, path: &str) -> Result<String> {
        self.disk
            .get(path)
            .cloned()
            .ok_or_else(|| ScanError::PrimitiveUnavailable("fake: no file".to_string()))
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

const DEPLOY: &str =
    r#"{"deployments":[{"booted":true,"checksum":"abc","packages":["opensnitch"]}]}"#;
const TAMPERED: &str = "..5......    /usr/bin/opensnitchd\n";
const TAMPERED_PATH: &str = "/usr/bin/opensnitchd";

/// Live-shaped: rpm and rpm-ostree answer, the base-tree primitive is
/// unavailable (plan OQ1), exactly as `LiveSystem` behaves today.
fn drift_facts(rpm_verify: &str) -> FakeFacts {
    FakeFacts {
        rpm_verify: Some(rpm_verify.to_string()),
        deployment_json: Some(DEPLOY.to_string()),
        owners: [(TAMPERED_PATH.to_string(), "opensnitch".to_string())]
            .into_iter()
            .collect(),
        ..healthy_facts()
    }
}

#[test]
fn file_drift_is_new_then_still_then_resolved_with_base_tree_partial() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();

    let r1 = run_privileged_scan(&drift_facts(TAMPERED), &store).unwrap();
    assert!(r1.skipped.iter().any(|s| s.check == "file_drift_base_tree"));
    let finding = r1.reconciled.new.iter().find(|f| f.path == TAMPERED_PATH);
    assert!(finding.is_some_and(|f| f.detail.contains("layered_pkg:opensnitch")));
    assert!(!r1.skipped.iter().any(|s| s.check == "file_drift"));

    let r2 = run_privileged_scan(&drift_facts(TAMPERED), &store).unwrap();
    assert!(r2.reconciled.new.is_empty());
    assert!(r2
        .reconciled
        .still_outstanding
        .iter()
        .any(|f| f.path == TAMPERED_PATH));

    // Restored: resolves even though the base-tree pass is unavailable.
    let r3 = run_privileged_scan(&drift_facts(""), &store).unwrap();
    assert!(r3
        .reconciled
        .resolved
        .iter()
        .any(|f| f.path == TAMPERED_PATH));
}

#[test]
fn a_truncated_run_never_resolves_findings_past_the_cap() {
    use scanner_privileged::checks::file_drift::MAX_DRIFT_PATHS;
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let late = "/usr/lib/late";
    let r1 = run_privileged_scan(&drift_facts("missing     /usr/lib/late\n"), &store).unwrap();
    assert!(r1.reconciled.new.iter().any(|f| f.path == late));

    // Scan 2: more than the cap of other anomalies, `late` is past the cap.
    let mut verify: String = (0..=MAX_DRIFT_PATHS)
        .map(|i| format!("missing     /usr/lib/g{i}\n"))
        .collect();
    verify.push_str("missing     /usr/lib/late\n");
    let r2 = run_privileged_scan(&drift_facts(&verify), &store).unwrap();
    assert!(
        r2.reconciled.resolved.is_empty(),
        "{:?}",
        r2.reconciled.resolved
    );
    assert!(r2
        .skipped
        .iter()
        .any(|s| s.check == "file_drift_incomplete"));

    // Scan 3: complete and clean -> now it may resolve.
    let r3 = run_privileged_scan(&drift_facts(""), &store).unwrap();
    assert!(r3.reconciled.resolved.iter().any(|f| f.path == late));
}

#[test]
fn a_failed_rpm_primitive_skips_file_drift_without_aborting_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let r1 = run_privileged_scan(&drift_facts(TAMPERED), &store).unwrap();
    assert!(r1.reconciled.new.iter().any(|f| f.path == TAMPERED_PATH));

    let mut broken = drift_facts(TAMPERED);
    broken.rpm_verify_io_error = true;
    broken.proc_modules.push_str("rk_hidden 1 0 - Live 0x0\n");
    broken.committed_kargs = String::new();
    let r2 = run_privileged_scan(&broken, &store).unwrap();
    let skip = r2.skipped.iter().find(|s| s.check == "file_drift");
    assert!(
        skip.is_some_and(|s| s.reason.contains("rpmdb locked")),
        "{:?}",
        r2.skipped
    );
    // The cheaper checks still ran and reported.
    assert!(r2
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "module:rk_hidden"));
    assert!(r2.reconciled.new.iter().any(|f| f.path == "kargs:quiet"));
    // A failed check never resolves its earlier finding.
    assert!(r2.reconciled.resolved.is_empty(), "{:?}", r2.reconciled);
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
    let report = run_privileged_scan(&facts, &store).unwrap();
    assert!(report
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "module:rk_hidden"));
    assert_eq!(report.informational.len(), 2); // secure boot + lockdown

    // Scan 2: same anomaly still present → still outstanding, not new.
    let report2 = run_privileged_scan(&facts, &store).unwrap();
    assert!(report2.reconciled.new.is_empty());
    assert!(report2
        .reconciled
        .still_outstanding
        .iter()
        .any(|f| f.path == "module:rk_hidden"));

    // Scan 3: module gone (rootkit removed) → resolved.
    let clean = healthy_facts();
    let report3 = run_privileged_scan(&clean, &store).unwrap();
    assert!(report3
        .reconciled
        .resolved
        .iter()
        .any(|f| f.path == "module:rk_hidden"));
}

#[test]
fn a_skipped_check_does_not_resolve_its_earlier_findings() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let lkm = "rootkit:chkrootkit:lkm";

    // Scan 1: chkrootkit reports an infection.
    let mut infected = healthy_facts();
    infected.chkrootkit = Some("/usr/sbin/chkrootkit".to_string());
    infected.chkrootkit_output = "Checking 'lkm'... INFECTED\n".to_string();
    let r1 = run_privileged_scan(&infected, &store).unwrap();
    assert!(r1.reconciled.new.iter().any(|f| f.path == lkm));

    // Scan 2: chkrootkit is gone (skipped) -> the finding must stay open.
    let r2 = run_privileged_scan(&healthy_facts(), &store).unwrap();
    assert!(r2.skipped.iter().any(|s| s.check == "rootkit_signature"));
    assert!(r2.reconciled.resolved.is_empty(), "{:?}", r2.reconciled);

    // Scan 3: chkrootkit runs and is clean -> now it resolves.
    let mut clean = healthy_facts();
    clean.chkrootkit = Some("/usr/sbin/chkrootkit".to_string());
    let r3 = run_privileged_scan(&clean, &store).unwrap();
    assert!(r3.reconciled.resolved.iter().any(|f| f.path == lkm));
}

#[test]
fn secure_boot_transition_flagged_only_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();

    // Scan 1: secure boot on → informational only.
    let r1 = run_privileged_scan(&healthy_facts(), &store).unwrap();
    assert!(!r1
        .reconciled
        .new
        .iter()
        .any(|f| f.path == "lockdown:secureboot"));

    // Scan 2: secure boot off → weakening transition → new anomaly.
    let mut off = healthy_facts();
    off.secure_boot = false;
    let r2 = run_privileged_scan(&off, &store).unwrap();
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
    let report = run_privileged_scan(&healthy_facts(), &store).unwrap();
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

const COMMIT: &str = "d39c44ff4c9cf2ef204e6e025429558ac2772758b62e458a2a7d068fdd793410";
const SUM_A: &str = "9b88d8cc9a6c591204bc4a12dd85488122834f756cc02f24ca3a1ddc5d49273f";
const SUM_B: &str = "989b825aa7d0db2e775b8cd6f45719fb557ebb5916a93b6d963b2577bb0e6279";

/// Healthy base-tree pass: `ostree` present, a 64-hex booted commit, and the
/// commit holds every path in `paths` with checksum A. `tampered` paths are
/// read from disk as B, the rest as A.
fn base_tree_facts(rpm_verify: &str, paths: &[String], tampered: &[&str]) -> FakeFacts {
    let mut facts = FakeFacts {
        rpm_verify: Some(rpm_verify.to_string()),
        deployment_json: Some(format!(
            r#"{{"deployments":[{{"booted":true,"checksum":"{COMMIT}"}}]}}"#
        )),
        ostree: true,
        ..healthy_facts()
    };
    for path in paths {
        let entry = CommitEntry {
            kind: scanner_privileged::checks::base_tree::EntryKind::File,
            checksum: SUM_A.to_string(),
        };
        facts.entries.insert(path.clone(), entry);
        let on_disk = if tampered.contains(&path.as_str()) {
            SUM_B
        } else {
            SUM_A
        };
        facts.disk.insert(path.clone(), on_disk.to_string());
    }
    facts
}

fn failing(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("S.5....T.    {p}\n"))
        .collect()
}

#[test]
fn a_healthy_run_reports_no_base_tree_skip_but_states_the_unowned_gap() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let paths = vec!["/usr/lib/os-release".to_string()];
    let facts = base_tree_facts(&failing(&paths), &paths, &[]);
    let r = run_privileged_scan(&facts, &store).unwrap();
    assert!(r.reconciled.new.is_empty(), "{:?}", r.reconciled.new);
    let checks: Vec<_> = r.skipped.iter().map(|s| s.check).collect();
    assert!(!checks.contains(&"file_drift_base_tree"), "{checks:?}");
    assert!(!checks.contains(&"file_drift_incomplete"), "{checks:?}");
    assert!(checks.contains(&"file_drift_unowned"), "{checks:?}");
}

#[test]
fn a_cap_flooded_degraded_run_resolves_once_a_healthy_run_matches_everything() {
    use scanner_privileged::checks::file_drift::MAX_DRIFT_PATHS;
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let paths: Vec<String> = (0..=MAX_DRIFT_PATHS)
        .map(|i| format!("/usr/lib64/f{i}.pyc"))
        .collect();
    // Before: no usable base-tree pass, so every rpm failure is anomalous.
    let mut degraded = base_tree_facts(&failing(&paths), &paths, &[]);
    degraded.ostree = false;
    let r1 = run_privileged_scan(&degraded, &store).unwrap();
    assert_eq!(r1.reconciled.new.len(), MAX_DRIFT_PATHS + 1);
    assert!(r1.reconciled.new.iter().any(|f| f.path == "/"));
    assert!(r1
        .skipped
        .iter()
        .any(|s| s.check == "file_drift_incomplete"));

    // After: the base-tree pass matches every candidate.
    let healthy = base_tree_facts(&failing(&paths), &paths, &[]);
    let r2 = run_privileged_scan(&healthy, &store).unwrap();
    assert_eq!(r2.reconciled.resolved.len(), MAX_DRIFT_PATHS + 1);
    assert!(r2.reconciled.new.is_empty());
    assert!(!r2
        .skipped
        .iter()
        .any(|s| s.check == "file_drift_incomplete"));
}

#[test]
fn a_tampered_base_file_is_new_then_still_then_resolved_when_restored() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let path = "/usr/bin/bash".to_string();
    let paths = [path.clone()];
    let tampered = base_tree_facts(&failing(&paths), &paths, &[&path]);

    let r1 = run_privileged_scan(&tampered, &store).unwrap();
    let new = r1.reconciled.new.iter().find(|f| f.path == path);
    assert!(new.is_some_and(|f| f.detail.contains(&format!("expected={SUM_A}"))));
    assert!(new.is_some_and(|f| f.detail.contains(&format!("actual={SUM_B}"))));
    let r2 = run_privileged_scan(&tampered, &store).unwrap();
    assert!(r2.reconciled.new.is_empty());
    assert!(r2
        .reconciled
        .still_outstanding
        .iter()
        .any(|f| f.path == path));

    let restored = base_tree_facts("", &paths, &[]);
    let r3 = run_privileged_scan(&restored, &store).unwrap();
    assert!(r3.reconciled.resolved.iter().any(|f| f.path == path));
}

#[test]
fn a_degraded_run_reports_why_and_never_resolves_a_healthy_finding() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let path = "/usr/bin/bash".to_string();
    let paths = [path.clone()];
    let tampered = base_tree_facts(&failing(&paths), &paths, &[&path]);
    run_privileged_scan(&tampered, &store).unwrap();

    let mut degraded = base_tree_facts(&failing(&paths), &paths, &[&path]);
    degraded.ostree = false;
    let r = run_privileged_scan(&degraded, &store).unwrap();
    let skip = r.skipped.iter().find(|s| s.check == "file_drift_base_tree");
    assert!(
        skip.is_some_and(|s| s.reason.contains("ostree")),
        "{:?}",
        r.skipped
    );
    assert!(
        r.reconciled.resolved.is_empty(),
        "{:?}",
        r.reconciled.resolved
    );
    assert!(r
        .reconciled
        .still_outstanding
        .iter()
        .any(|f| f.path == path));
}

#[test]
fn exactly_one_rpm_ostree_status_read_per_scan() {
    let dir = tempfile::tempdir().unwrap();
    let store = ScanStore::open(&dir.path().join("scans.db")).unwrap();
    let paths = vec!["/usr/lib/os-release".to_string()];
    let facts = base_tree_facts(&failing(&paths), &paths, &[]);
    run_privileged_scan(&facts, &store).unwrap();
    assert_eq!(facts.status_reads.get(), 1);
}
