//! Userspace-tier scan orchestration: run every check, funnel their
//! anomalies through the [`ScanStore`] reconcile, and assemble the
//! three-bucket [`ScanReport`] (baseline design §4).

use std::path::PathBuf;

use chrono::Utc;
use thiserror::Error;

use crate::checks::{
    flatpak::FlatpakCheck, listeners::ListenersCheck, outbound::OutboundCheck,
    shell_rc::ShellRcCheck, systemd_user::SystemdUserCheck, Check, CheckError, CheckOutcome,
    ScanContext,
};
use crate::deployment::DeploymentState;
use crate::finding::{Anomaly, ScanReport};
use crate::inspector::SystemInspector;
use crate::stores::scans::ScanStore;
use crate::stores::StoreError;

/// Sentinel deployment id used when the host is not an rpm-ostree system, so
/// scan history is still coherent on a non-atomic dev box.
pub const NO_DEPLOYMENT: &str = "no-rpm-ostree";

/// The tier label recorded on scan runs and findings.
pub const TIER: &str = "userspace";

#[derive(Debug, Error)]
pub enum ScannerError {
    #[error(transparent)]
    Check(#[from] CheckError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Userspace scanner: owns the check set and drives one scan run.
pub struct Scanner<'a> {
    sys: &'a dyn SystemInspector,
    scans: &'a ScanStore,
    checks: Vec<Box<dyn Check + 'a>>,
}

impl<'a> Scanner<'a> {
    /// Construct with an explicit check set (used by tests).
    pub fn new(
        sys: &'a dyn SystemInspector,
        scans: &'a ScanStore,
        checks: Vec<Box<dyn Check + 'a>>,
    ) -> Self {
        Self { sys, scans, checks }
    }

    /// Construct with the full userspace check set, in report order.
    pub fn with_default_checks(sys: &'a dyn SystemInspector, scans: &'a ScanStore) -> Self {
        Self::new(
            sys,
            scans,
            vec![
                Box::new(SystemdUserCheck),
                Box::new(ShellRcCheck),
                Box::new(FlatpakCheck),
                Box::new(ListenersCheck),
                Box::new(OutboundCheck),
            ],
        )
    }

    /// Run one userspace scan.
    ///
    /// `home` is the user's home directory; `deployment` is the active
    /// rpm-ostree deployment if one was resolved (on a non-atomic host this
    /// is `None` and a sentinel checksum is recorded).
    pub fn run(
        &self,
        home: PathBuf,
        deployment: Option<DeploymentState>,
    ) -> Result<ScanReport, ScannerError> {
        let deployment_checksum = deployment
            .as_ref()
            .map(|d| d.checksum.clone())
            .unwrap_or_else(|| NO_DEPLOYMENT.to_string());

        let scan_id = self
            .scans
            .begin_scan(&deployment_checksum, TIER, Utc::now())?;

        let ctx = ScanContext {
            sys: self.sys,
            deployment: deployment.as_ref(),
            home,
            scans: self.scans,
        };

        let mut all_anomalies: Vec<Anomaly> = Vec::new();
        let mut baselined_surfaces = Vec::new();
        let mut skipped_checks = Vec::new();
        let mut deferred_checks = Vec::new();

        for check in &self.checks {
            match check.run(&ctx)? {
                CheckOutcome::Ran {
                    mut anomalies,
                    baselined,
                } => {
                    if baselined {
                        baselined_surfaces.push(check.surface().to_string());
                    }
                    all_anomalies.append(&mut anomalies);
                }
                CheckOutcome::Skipped { reason } => {
                    skipped_checks.push(format!("{}: {reason}", check.name()));
                }
                CheckOutcome::Deferred { reason } => {
                    deferred_checks.push(format!("{}: {reason}", check.name()));
                }
            }
        }

        let reconcile = self.scans.reconcile(scan_id, &all_anomalies)?;
        self.scans.finish_scan(scan_id, Utc::now())?;

        Ok(ScanReport {
            scan_id,
            tier: TIER.to_string(),
            deployment_checksum,
            baselined_surfaces,
            skipped_checks,
            deferred_checks,
            new: reconcile.new,
            still_outstanding: reconcile.still_outstanding,
            resolved: reconcile.resolved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockInspector;

    #[test]
    fn default_scan_defers_outbound_and_baselines_on_first_run() {
        // Minimal host: only shell rc present, no flatpak/ss/rpm-ostree.
        let sys = MockInspector::new().with_file("/home/tester/.bashrc", "echo hi\n");
        let scans = ScanStore::open_in_memory().unwrap();
        let scanner = Scanner::with_default_checks(&sys, &scans);

        let report = scanner.run(PathBuf::from("/home/tester"), None).unwrap();

        // First run: everything is baseline, nothing actionable.
        assert!(report.is_clean());
        assert_eq!(report.deployment_checksum, NO_DEPLOYMENT);
        // Outbound is reported as deferred, explicitly.
        assert_eq!(report.deferred_checks.len(), 1);
        assert!(report.deferred_checks[0].starts_with("outbound:"));
        // flatpak + listeners skipped (binaries absent in this mock).
        assert!(report
            .skipped_checks
            .iter()
            .any(|s| s.starts_with("flatpak:")));
        assert!(report
            .skipped_checks
            .iter()
            .any(|s| s.starts_with("listeners:")));
        // shell-rc + systemd-user baselined.
        assert!(report.baselined_surfaces.iter().any(|s| s == "shell-rc"));
    }

    #[test]
    fn second_scan_flags_new_anomaly_then_marks_resolved() {
        let scans = ScanStore::open_in_memory().unwrap();

        // Run 1: baseline with a benign .bashrc.
        let sys1 = MockInspector::new().with_file("/home/tester/.bashrc", "echo hi\n");
        Scanner::with_default_checks(&sys1, &scans)
            .run(PathBuf::from("/home/tester"), None)
            .unwrap();

        // Run 2: .bashrc tampered -> new finding.
        let sys2 = MockInspector::new().with_file("/home/tester/.bashrc", "echo hi\nmalware\n");
        let r2 = Scanner::with_default_checks(&sys2, &scans)
            .run(PathBuf::from("/home/tester"), None)
            .unwrap();
        assert_eq!(r2.new.len(), 1);
        assert!(r2.new[0].path.ends_with(".bashrc"));

        // Run 3: still tampered -> still outstanding.
        let r3 = Scanner::with_default_checks(&sys2, &scans)
            .run(PathBuf::from("/home/tester"), None)
            .unwrap();
        assert_eq!(r3.still_outstanding.len(), 1);
        assert!(r3.new.is_empty());

        // Run 4: reverted -> resolved.
        let sys4 = MockInspector::new().with_file("/home/tester/.bashrc", "echo hi\n");
        let r4 = Scanner::with_default_checks(&sys4, &scans)
            .run(PathBuf::from("/home/tester"), None)
            .unwrap();
        assert_eq!(r4.resolved.len(), 1);
        assert!(r4.is_clean());
    }
}
