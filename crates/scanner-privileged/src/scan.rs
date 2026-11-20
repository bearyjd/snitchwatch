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

use crate::checks::{file_drift, kargs, lockdown, modules, rootkit};
use crate::error::{Result, ScanError};
use crate::facts::SystemFacts;
use crate::model::{CheckType, Classification, Finding, SkippedCheck};
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

/// Wire form of `--json` output. `scanner-broker::Report` and the Kirigami
/// scanner summary consume it; `tests/report_contract.rs` pins the shape.
pub fn report_json(report: &ScanReport) -> serde_json::Value {
    let pairs = |v: &[crate::store::ReconciledFinding]| {
        v.iter()
            .map(|f| serde_json::json!({"path": f.path, "detail": f.detail}))
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "scan_id": report.scan_id,
        "new": pairs(&report.reconciled.new),
        "still_outstanding": pairs(&report.reconciled.still_outstanding),
        "resolved": report.reconciled.resolved.iter().map(|f| serde_json::json!({"path": f.path})).collect::<Vec<_>>(),
        "informational": report.informational.iter().map(|f| serde_json::json!({"path": f.path, "detail": f.detail})).collect::<Vec<_>>(),
        "skipped": report.skipped.iter().map(|s| serde_json::json!({"check": s.check, "reason": s.reason})).collect::<Vec<_>>(),
    })
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
    // Checks that ran to completion; only these may resolve prior findings.
    let mut completed: Vec<CheckType> = Vec::new();

    // 1. File-integrity/drift check (rpm -Va, classified per baseline spec §1).
    //    A failed primitive is a visible skip, never an abort: the cheaper
    //    checks below must still run (privileged spec §3).
    run_file_drift_step(facts, &mut anomalies, &mut skipped, &mut completed);

    // 2. Kargs drift (cheap).
    match run_kargs(facts) {
        Ok(mut f) => {
            anomalies.append(&mut f);
            completed.push(CheckType::KargsDrift);
        }
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "kargs_drift",
            reason,
        }),
        Err(e) => return Err(e),
    }

    // 3. Loaded-module audit (cheap).
    match modules::audit_modules(facts) {
        Ok(mut f) => {
            anomalies.append(&mut f);
            completed.push(CheckType::ModuleAnomaly);
        }
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "module_anomaly",
            reason,
        }),
        Err(e) => return Err(e),
    }

    // 4. Secure Boot / lockdown state (cheap). Transition detection reads and
    //    updates the state_snapshots table.
    if run_state_checks(
        facts,
        store,
        &mut anomalies,
        &mut informational,
        &mut skipped,
    )? {
        completed.push(CheckType::LockdownState);
    }

    // 5. chkrootkit (slowest — last).
    match rootkit::scan_rootkits(facts) {
        Ok(rootkit::RootkitOutcome::Scanned(mut f)) => {
            anomalies.append(&mut f);
            completed.push(CheckType::RootkitSignature);
        }
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

    let reconciled = store.reconcile(scan_id, &anomalies, &completed)?;
    store.finish_scan(scan_id)?;

    Ok(ScanReport {
        scan_id,
        reconciled,
        informational,
        skipped,
    })
}

/// Run the file-drift check. It counts as completed (so its earlier findings
/// may resolve) once `rpm -Va` was read and understood, even while the
/// base-tree pass is unavailable (plan, interim rule: until plan OQ1 no
/// base-tree finding can exist to be wrongly resolved). Lines `rpm -Va` printed
/// that were not understood fail closed: reported, and nothing resolves.
fn run_file_drift_step(
    facts: &dyn SystemFacts,
    anomalies: &mut Vec<Finding>,
    skipped: &mut Vec<SkippedCheck>,
    completed: &mut Vec<CheckType>,
) {
    let outcome = match file_drift::run_file_drift(facts) {
        Ok(outcome) => outcome,
        Err(error) => {
            let reason = match error {
                ScanError::PrimitiveUnavailable(reason) => reason,
                other => other.to_string(),
            };
            skipped.push(SkippedCheck {
                check: "file_drift",
                reason,
            });
            return;
        }
    };
    anomalies.extend(outcome.findings);
    let incomplete = outcome.incomplete.is_some();
    if let Some(reason) = outcome.incomplete {
        skipped.push(SkippedCheck {
            check: "file_drift_incomplete",
            reason: format!("{reason}; prior file_drift findings kept open"),
        });
    }
    if let Some(reason) = outcome.base_tree_unavailable {
        skipped.push(SkippedCheck {
            check: "file_drift_base_tree",
            reason,
        });
    }
    if outcome.unparsed_lines > 0 {
        skipped.push(SkippedCheck {
            check: "file_drift_unparsed",
            reason: format!(
                "{} rpm -Va output lines were not understood; prior file_drift findings kept open",
                outcome.unparsed_lines
            ),
        });
    } else if !incomplete {
        completed.push(CheckType::FileDrift);
    }
}

fn run_kargs(facts: &dyn SystemFacts) -> Result<Vec<Finding>> {
    let cmdline = facts.proc_cmdline()?;
    let committed = facts.committed_kargs()?;
    Ok(kargs::diff_kargs(&cmdline, &committed))
}

/// Secure Boot + lockdown transition checks. Anomalous transitions go to
/// `anomalies`; the current-state informational findings go to
/// `informational`. Each dimension is independent: an unavailable primitive
/// for one (e.g. `mokutil` absent) skips only that dimension. Returns whether
/// both dimensions ran, i.e. whether `LockdownState` findings may resolve.
fn run_state_checks(
    facts: &dyn SystemFacts,
    store: &ScanStore,
    anomalies: &mut Vec<Finding>,
    informational: &mut Vec<Finding>,
    skipped: &mut Vec<SkippedCheck>,
) -> Result<bool> {
    let mut ran = 0;
    // Secure Boot.
    match facts.secure_boot_enabled() {
        Ok(enabled) => {
            let prior = store.get_state_snapshot(lockdown::SNAP_SECURE_BOOT)?;
            let eval = lockdown::eval_secure_boot(prior.as_deref(), enabled);
            store.set_state_snapshot(eval.snapshot_key, &eval.new_value)?;
            route(eval.finding, anomalies, informational);
            ran += 1;
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
            ran += 1;
        }
        Err(ScanError::PrimitiveUnavailable(reason)) => skipped.push(SkippedCheck {
            check: "lockdown",
            reason,
        }),
        Err(e) => return Err(e),
    }

    Ok(ran == 2)
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
        // file_drift (rpm/rpm-ostree unconfigured here) + chkrootkit skipped.
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

    const DEPLOY: &str = r#"{"deployments":[{"booted":true,"checksum":"c","packages":[]}]}"#;

    fn drift_facts(verify: &str) -> SyntheticFacts {
        base_facts()
            .with_deployment_json(DEPLOY)
            .with_rpm_verify(verify)
            .with_owner("/usr/bin/bash", "bash")
    }

    #[test]
    fn not_understood_rpm_output_fails_closed_and_resolves_nothing() {
        let store = ScanStore::open_in_memory().unwrap();
        let tampered = drift_facts("S.5....T.    /usr/bin/bash\n");
        let r1 = run_privileged_scan(&tampered, &store, "dep1").unwrap();
        assert!(r1.reconciled.new.iter().any(|f| f.path == "/usr/bin/bash"));
        assert!(!r1.skipped.iter().any(|s| s.check == "file_drift"));

        let r2 = run_privileged_scan(&drift_facts("error: odd\n"), &store, "dep1").unwrap();
        assert!(r2.skipped.iter().any(|s| s.check == "file_drift_unparsed"));
        assert!(r2.reconciled.resolved.is_empty(), "{:?}", r2.reconciled);
    }
}
