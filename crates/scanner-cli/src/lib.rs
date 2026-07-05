//! Wiring for the userspace scanner CLI. The orchestration lives here (not
//! in `main.rs`) so tests can exercise it without spawning a subprocess,
//! mirroring `snitchwatch-bridge-cli`'s `run`/`main` split.

use std::path::PathBuf;

use anyhow::{Context, Result};
use scanner_core::deployment::{self, DeployError};
use scanner_core::inspector::{RealSystem, SystemInspector};
use scanner_core::stores::scans::ScanStore;
use scanner_core::{DeploymentState, ScanReport, Scanner};

/// Resolved runtime configuration for a scan.
#[derive(Debug, Clone)]
pub struct ScannerConfig {
    /// The user whose surfaces are scanned.
    pub home: PathBuf,
    /// Where `scans.db` (the userspace-owned history/findings store) lives.
    pub scans_db: PathBuf,
}

impl ScannerConfig {
    /// Build config from the environment.
    ///
    /// * `home` — `$HOME`.
    /// * `scans.db` — `$SNITCHWATCH_SCANNER_STATE_DIR`, else
    ///   `$XDG_STATE_HOME/snitchwatch-scanner`, else
    ///   `$HOME/.local/state/snitchwatch-scanner` (baseline design §4's
    ///   userspace-owned path).
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;

        let state_dir = if let Some(dir) = std::env::var_os("SNITCHWATCH_SCANNER_STATE_DIR") {
            PathBuf::from(dir)
        } else if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
            PathBuf::from(xdg).join("snitchwatch-scanner")
        } else {
            home.join(".local/state/snitchwatch-scanner")
        };

        Ok(Self {
            home,
            scans_db: state_dir.join("scans.db"),
        })
    }
}

/// Run one userspace scan against the real host, returning the report.
pub fn run(config: &ScannerConfig) -> Result<ScanReport> {
    run_with(&RealSystem, config)
}

/// Run a scan with an injectable inspector (tests pass a mock).
pub fn run_with(sys: &dyn SystemInspector, config: &ScannerConfig) -> Result<ScanReport> {
    if let Some(parent) = config.scans_db.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create scanner state dir {}", parent.display()))?;
    }

    let deployment = resolve_deployment(sys);
    let scans = ScanStore::open(&config.scans_db)
        .with_context(|| format!("failed to open scans.db at {}", config.scans_db.display()))?;

    let scanner = Scanner::with_default_checks(sys, &scans);
    scanner
        .run(config.home.clone(), deployment)
        .context("userspace scan failed")
}

/// Resolve the active deployment, degrading gracefully to `None` on a
/// non-atomic host (a dev box without rpm-ostree) instead of failing.
fn resolve_deployment(sys: &dyn SystemInspector) -> Option<DeploymentState> {
    match deployment::probe_deployment(sys) {
        Ok(state) => Some(state),
        Err(DeployError::NotAtomic) => {
            tracing::info!("rpm-ostree not present; running standalone without deployment context");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to probe rpm-ostree deployment; continuing");
            None
        }
    }
}

/// Serialize a report to pretty JSON.
pub fn report_to_json(report: &ScanReport) -> Result<String> {
    serde_json::to_string_pretty(report).context("failed to serialize scan report")
}

#[cfg(test)]
mod tests {
    use super::*;
    use scanner_core::testkit::MockInspector;
    use tempfile::tempdir;

    #[test]
    fn run_with_mock_produces_report_and_persists_across_runs() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".bashrc"), "echo hi\n").unwrap();

        let config = ScannerConfig {
            home: home.clone(),
            scans_db: dir.path().join("state/scans.db"),
        };

        // First run reads the real filesystem for shell rc, so use RealSystem
        // here to exercise the real read path end-to-end.
        let sys = RealSystem;
        let r1 = run_with(&sys, &config).unwrap();
        assert!(r1.is_clean());
        assert!(r1
            .deferred_checks
            .iter()
            .any(|c| c.starts_with("outbound:")));

        // Tamper and rescan -> a new finding, proving persistence across
        // separate store opens.
        std::fs::write(home.join(".bashrc"), "echo hi\nmalware\n").unwrap();
        let r2 = run_with(&sys, &config).unwrap();
        assert_eq!(r2.new.len(), 1);
        assert!(r2.new[0].path.ends_with(".bashrc"));
    }

    #[test]
    fn mock_inspector_drives_a_full_report() {
        let dir = tempdir().unwrap();
        let config = ScannerConfig {
            home: PathBuf::from("/home/tester"),
            scans_db: dir.path().join("scans.db"),
        };
        let sys = MockInspector::new();
        let report = run_with(&sys, &config).unwrap();
        assert_eq!(report.tier, "userspace");
        assert_eq!(report.deployment_checksum, "no-rpm-ostree");
        // JSON round-trips.
        let json = report_to_json(&report).unwrap();
        assert!(json.contains("\"tier\": \"userspace\""));
    }
}
