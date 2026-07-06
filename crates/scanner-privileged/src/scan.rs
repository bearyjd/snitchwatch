//! Orchestration: run all privileged-tier sub-checks as ordered sub-steps of
//! ONE invocation (privileged spec §3), producing one scan report.
//!
//! Ordering (spec §3): file-integrity → kargs drift → module audit →
//! Secure Boot/lockdown → chkrootkit (slowest, last, so a cancelled
//! long-running scan still records the cheaper checks' results).
//!
//! A sub-check whose required primitive is unavailable is recorded as a
//! `SkippedCheck` and the scan continues — a degraded scan is visibly
//! degraded, never silently "all clear".

use crate::checks::{kargs, lockdown, modules, rootkit};
use crate::error::{Result, ScanError};
use crate::facts::SystemFacts;
use crate::model::{Classification, Finding, SkippedCheck};
use crate::store::{ReconcileReport, ScanStore, TIER_PRIVILEGED};

/// Full result of one privileged-tier invocation.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub scan_id: i64,
    /// Anomalous findings, reconciled into new / still-outstanding / resolved.
    pub reconciled: ReconcileReport,
    /// Informational (non-anomaly) findings, e.g. current Secure Boot state.
    pub informational: Vec<Finding>,
    /// Sub-checks that could not run (missing primitive / not installed).
    pub skipped: Vec<SkippedCheck>,
}

/// Run the ordered privileged-tier scan against `facts`, persisting results
/// to `store`. `deployment_checksum` tags the scan run (from
/// `rpm-ostree status --json`; caller supplies it).
pub fn run_privileged_scan(
    facts: &dyn SystemFacts,
    store: &ScanStore,
    deployment_checksum: &str,
) -> Result<ScanReport> {
    let scan_id = store.start_scan(deployment_checksum, TIER_PRIVILEGED)?;

    let mut anomalies: Vec<Finding> = Vec::new();
    let mut informational: Vec<Finding> = Vec::new();
    let mut skipped: Vec<SkippedCheck> = Vec::new();

    // 1. File-integrity/drift check — specified by the baseline design doc and
    //    owned by the shared baseline cache (baseline.db), which is Phase 5's
    //    sibling-branch work not yet merged here. Recorded as deferred so the
    //    ordering/integration point is explicit and not silently dropped.
    skipped.push(SkippedCheck {
        check: "file_drift",
        reason: "delegated to shared baseline cache (baseline.db); lands when Phase 5 merges"
            .to_string(),
    });

    // 2. Kargs drift (cheap).
    match run_kargs(facts) {
        Ok(mut f) => anomalies.append(&mut f),
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "kargs_drift",
            reason,
        }),
        Err(e) => return Err(e),
    }

    // 3. Loaded-module audit (cheap).
    match modules::audit_modules(facts) {
        Ok(mut f) => anomalies.append(&mut f),
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "module_anomaly",
            reason,
        }),
        Err(e) => return Err(e),
    }

    // 4. Secure Boot / lockdown state (cheap). Transition detection reads and
    //    updates the state_snapshots table.
    run_state_checks(
        facts,
        store,
        &mut anomalies,
        &mut informational,
        &mut skipped,
    )?;

    // 5. chkrootkit (slowest — last).
    match rootkit::scan_rootkits(facts) {
        Ok(rootkit::RootkitOutcome::Scanned(mut f)) => anomalies.append(&mut f),
        Ok(rootkit::RootkitOutcome::Skipped { reason }) => skipped.push(SkippedCheck {
            check: "rootkit_signature",
            reason,
        }),
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "rootkit_signature",
            reason,
        }),
        Err(e) => return Err(e),
    }

    let reconciled = store.reconcile(scan_id, &anomalies)?;
    store.finish_scan(scan_id)?;

    Ok(ScanReport {
        scan_id,
        reconciled,
        informational,
        skipped,
    })
}

fn run_kargs(facts: &dyn SystemFacts) -> Result<Vec<Finding>> {
    let cmdline = facts.proc_cmdline()?;
    let committed = facts.committed_kargs()?;
    Ok(kargs::diff_kargs(&cmdline, &committed))
}

/// Secure Boot + lockdown transition checks. Anomalous transitions go to
/// `anomalies`; the current-state informational findings go to
/// `informational`. Each dimension is independent: an unavailable primitive
/// for one (e.g. `mokutil` absent) skips only that dimension.
fn run_state_checks(
    facts: &dyn SystemFacts,
    store: &ScanStore,
    anomalies: &mut Vec<Finding>,
    informational: &mut Vec<Finding>,
    skipped: &mut Vec<SkippedCheck>,
) -> Result<()> {
    // Secure Boot.
    match facts.secure_boot_enabled() {
        Ok(enabled) => {
            let prior = store.get_state_snapshot(lockdown::SNAP_SECURE_BOOT)?;
            let eval = lockdown::eval_secure_boot(prior.as_deref(), enabled);
            store.set_state_snapshot(eval.snapshot_key, &eval.new_value)?;
            route(eval.finding, anomalies, informational);
        }
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "secure_boot",
            reason,
        }),
        Err(e) => return Err(e),
    }

    // Kernel lockdown.
    match facts.lockdown_state() {
        Ok(state) => {
            let prior = store.get_state_snapshot(lockdown::SNAP_LOCKDOWN)?;
            let eval = lockdown::eval_lockdown(prior.as_deref(), &state);
            store.set_state_snapshot(eval.snapshot_key, &eval.new_value)?;
            route(eval.finding, anomalies, informational);
        }
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "lockdown",
            reason,
        }),
        Err(e) => return Err(e),
    }

    Ok(())
}

fn route(finding: Finding, anomalies: &mut Vec<Finding>, informational: &mut Vec<Finding>) {
    match finding.classification {
        Classification::Anomalous => anomalies.push(finding),
        Classification::Informational => informational.push(finding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::testing::SyntheticFacts;

    fn base_facts() -> SyntheticFacts {
        SyntheticFacts::new()
            .with_proc_cmdline("BOOT_IMAGE=/vmlinuz root=UUID=x quiet")
            .with_committed_kargs("quiet")
            .with_proc_modules("xfs 1 0 - Live 0x0\n")
            .with_base_tree_modules(["xfs"])
            .with_lockdown("integrity")
            .with_secure_boot(true)
    }

    #[test]
    fn clean_system_first_scan_only_informational_and_skips() {
        let store = ScanStore::open_in_memory().unwrap();
        let report = run_privileged_scan(&base_facts(), &store, "dep1").unwrap();
        // No anomalies.
        assert!(report.reconciled.new.is_empty());
        // Two informational state findings (secure boot + lockdown).
        assert_eq!(report.informational.len(), 2);
        // file_drift + chkrootkit (not installed) skipped.
        let skipped: Vec<_> = report.skipped.iter().map(|s| s.check).collect();
        assert!(skipped.contains(&"file_drift"));
        assert!(skipped.contains(&"rootkit_signature"));
    }

    #[test]
    fn kargs_drift_surfaces_as_new_anomaly() {
        let facts = base_facts().with_committed_kargs(""); // nothing committed → quiet is drift
        let store = ScanStore::open_in_memory().unwrap();
        let report = run_privileged_scan(&facts, &store, "dep1").unwrap();
        assert!(report
            .reconciled
            .new
            .iter()
            .any(|f| f.path == "kargs:quiet"));
    }

    #[test]
    fn anomalous_module_surfaces_as_new_anomaly() {
        let facts =
            base_facts().with_proc_modules("xfs 1 0 - Live 0x0\nrk_hidden 1 0 - Live 0x0\n");
        let store = ScanStore::open_in_memory().unwrap();
        let report = run_privileged_scan(&facts, &store, "dep1").unwrap();
        assert!(report
            .reconciled
            .new
            .iter()
            .any(|f| f.path == "module:rk_hidden"));
    }

    #[test]
    fn secure_boot_disable_transition_flags_between_scans() {
        let store = ScanStore::open_in_memory().unwrap();
        // Scan 1: enabled.
        run_privileged_scan(&base_facts(), &store, "dep1").unwrap();
        // Scan 2: now disabled → weakening transition → anomaly.
        let facts2 = base_facts().with_secure_boot(false);
        let report = run_privileged_scan(&facts2, &store, "dep1").unwrap();
        assert!(report
            .reconciled
            .new
            .iter()
            .any(|f| f.path == "lockdown:secureboot"));
    }

    #[test]
    fn chkrootkit_infection_surfaces() {
        let facts =
            base_facts().with_chkrootkit("/usr/sbin/chkrootkit", "Checking 'lkm'... INFECTED\n");
        let store = ScanStore::open_in_memory().unwrap();
        let report = run_privileged_scan(&facts, &store, "dep1").unwrap();
        assert!(report
            .reconciled
            .new
            .iter()
            .any(|f| f.path == "rootkit:chkrootkit:lkm"));
    }

    #[test]
    fn unavailable_kargs_primitive_skips_not_fails() {
        // No proc_cmdline configured → PrimitiveUnavailable → skipped.
        let facts = SyntheticFacts::new()
            .with_proc_modules("xfs 1 0 - Live 0x0\n")
            .with_base_tree_modules(["xfs"])
            .with_lockdown("none")
            .with_secure_boot(false);
        let store = ScanStore::open_in_memory().unwrap();
        let report = run_privileged_scan(&facts, &store, "dep1").unwrap();
        assert!(report.skipped.iter().any(|s| s.check == "kargs_drift"));
    }
}
