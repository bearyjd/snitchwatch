//! Userspace-tier checks and the shared machinery they run on.
//!
//! Each check inspects one *user-writable surface* and reports anomalies.
//! None require elevated privileges — that is the defining property of the
//! userspace tier (baseline design §2). The privilege-justifying full-`/usr`
//! hash pass is deliberately absent here and belongs to Phase 6.
//!
//! Surfaces implemented:
//! * [`systemd_user`] — unexpected systemd `--user` units + autostart entries
//! * [`shell_rc`] — shell rc file modifications
//! * [`flatpak`] — Flatpak permission (override) drift
//! * [`listeners`] — unexpected listeners on user-reachable ports
//! * [`outbound`] — **deferred** (needs Component A's connection log)

pub mod flatpak;
pub mod listeners;
pub mod outbound;
pub mod shell_rc;
pub mod systemd_user;

use std::path::PathBuf;

use chrono::Utc;
use thiserror::Error;

use crate::deployment::DeploymentState;
use crate::finding::Anomaly;
use crate::inspector::SystemInspector;
use crate::stores::scans::ScanStore;
use crate::stores::StoreError;

/// Stable SHA-256 hex digest of a string — used as the recorded baseline
/// value for content surfaces (rc files, unit files) so that modification,
/// not just appearance, is detected.
pub(crate) fn sha256_hex(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Marker item written into a surface's baseline on first capture so that a
/// surface which legitimately started *empty* is still recognised as
/// initialised — otherwise the first item to ever appear would be
/// mistaken for a trust-on-first-use capture instead of a real anomaly.
pub(crate) const INIT_MARKER: &str = "\u{1}surface-initialised";

#[derive(Debug, Error)]
pub enum CheckError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("inspector error: {0}")]
    Inspect(#[from] crate::inspector::InspectError),
}

/// Everything a check needs to run.
pub struct ScanContext<'a> {
    pub sys: &'a dyn SystemInspector,
    pub deployment: Option<&'a DeploymentState>,
    /// The user's home directory (surfaces like shell rc / autostart live
    /// under it). Injected so tests can point it anywhere.
    pub home: PathBuf,
    pub scans: &'a ScanStore,
}

/// One current item observed on a surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceItem {
    /// Stable identity within the surface (becomes the [`Anomaly::path`]).
    pub key: String,
    /// Value compared against the recorded baseline (checksum, serialized
    /// permission set, owning exe, …). A change is an anomaly.
    pub value: String,
    /// Human-readable detail for the finding.
    pub detail: String,
}

/// Result of running a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Check ran; may have produced anomalies. `baselined` is true when this
    /// run captured the surface's trust-on-first-use baseline (in which case
    /// `anomalies` is empty by construction).
    Ran {
        anomalies: Vec<Anomaly>,
        baselined: bool,
    },
    /// A required host tool was absent, so the check was skipped.
    Skipped { reason: String },
    /// Deliberately deferred to a future release.
    Deferred { reason: String },
}

/// A userspace surface check.
pub trait Check {
    /// Stable check name, recorded on findings (`findings.check_name`).
    fn name(&self) -> &'static str;

    /// The surface key used for the trust-on-first-use baseline table.
    fn surface(&self) -> &'static str;

    fn run(&self, ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError>;
}

/// Shared trust-on-first-use diff used by the surface checks.
///
/// * First run for a surface (not yet initialised): record the current items
///   as the baseline and report nothing (`baselined = true`).
/// * Later runs: an item whose key is absent from the baseline (newly
///   appeared) or whose value differs from the baseline (changed) is an
///   anomaly. Items that disappeared are not anomalies (a removed autostart
///   entry or a closed port is not suspicious).
pub(crate) fn diff_surface(
    ctx: &ScanContext<'_>,
    check_name: &str,
    surface: &str,
    current: Vec<SurfaceItem>,
) -> Result<CheckOutcome, CheckError> {
    let initialised = !ctx.scans.surface_baseline_is_empty(surface)?;

    if !initialised {
        let mut items: Vec<(String, String)> = current
            .iter()
            .map(|i| (i.key.clone(), i.value.clone()))
            .collect();
        items.push((INIT_MARKER.to_string(), "1".to_string()));
        ctx.scans
            .capture_surface_baseline(surface, &items, Utc::now())?;
        return Ok(CheckOutcome::Ran {
            anomalies: Vec::new(),
            baselined: true,
        });
    }

    let baseline = ctx.scans.load_surface_baseline(surface)?;
    let mut anomalies = Vec::new();
    for item in current {
        match baseline.get(&item.key) {
            None => anomalies.push(Anomaly::new(check_name, item.key, item.detail)),
            Some(recorded) if *recorded != item.value => {
                anomalies.push(Anomaly::new(check_name, item.key, item.detail))
            }
            Some(_) => {}
        }
    }
    Ok(CheckOutcome::Ran {
        anomalies,
        baselined: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockInspector;

    fn ctx_with<'a>(sys: &'a MockInspector, scans: &'a ScanStore) -> ScanContext<'a> {
        ScanContext {
            sys,
            deployment: None,
            home: PathBuf::from("/home/tester"),
            scans,
        }
    }

    fn item(key: &str, value: &str) -> SurfaceItem {
        SurfaceItem {
            key: key.to_string(),
            value: value.to_string(),
            detail: format!("detail for {key}"),
        }
    }

    #[test]
    fn first_run_baselines_and_reports_nothing() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        let ctx = ctx_with(&sys, &scans);
        let out = diff_surface(&ctx, "s", "surface-a", vec![item("k1", "v1")]).unwrap();
        assert_eq!(
            out,
            CheckOutcome::Ran {
                anomalies: vec![],
                baselined: true
            }
        );
    }

    #[test]
    fn empty_first_run_still_initialises_so_later_items_are_anomalies() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        let ctx = ctx_with(&sys, &scans);
        // First run: nothing on the surface.
        let out1 = diff_surface(&ctx, "s", "surface-a", vec![]).unwrap();
        assert!(matches!(
            out1,
            CheckOutcome::Ran {
                baselined: true,
                ..
            }
        ));
        // Second run: an item appears -> must be an anomaly, not a fresh
        // baseline capture.
        let out2 = diff_surface(&ctx, "s", "surface-a", vec![item("k1", "v1")]).unwrap();
        match out2 {
            CheckOutcome::Ran {
                anomalies,
                baselined,
            } => {
                assert!(!baselined);
                assert_eq!(anomalies.len(), 1);
                assert_eq!(anomalies[0].path, "k1");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn changed_value_is_an_anomaly() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        let ctx = ctx_with(&sys, &scans);
        diff_surface(&ctx, "s", "surface-a", vec![item("k1", "v1")]).unwrap();
        let out = diff_surface(&ctx, "s", "surface-a", vec![item("k1", "v2")]).unwrap();
        match out {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert_eq!(anomalies[0].path, "k1");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unchanged_and_removed_items_are_not_anomalies() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        let ctx = ctx_with(&sys, &scans);
        diff_surface(
            &ctx,
            "s",
            "surface-a",
            vec![item("k1", "v1"), item("k2", "v2")],
        )
        .unwrap();
        // k1 unchanged, k2 removed.
        let out = diff_surface(&ctx, "s", "surface-a", vec![item("k1", "v1")]).unwrap();
        match out {
            CheckOutcome::Ran { anomalies, .. } => assert!(anomalies.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }
}
