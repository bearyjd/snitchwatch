//! Active rpm-ostree deployment state — the "cheap metadata" of baseline
//! design §2 that is recomputed fresh on every scan (never cached).
//!
//! We delegate entirely to `rpm-ostree status --json` and only *parse* its
//! output — we never re-derive package/commit state ourselves.

use serde::Deserialize;
use thiserror::Error;

use crate::inspector::{InspectError, SystemInspector};

/// The active (booted) deployment's identity and layered-package set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentState {
    /// The booted deployment's checksum — the cache key for the privileged
    /// tier's full-hash baseline (baseline design §2).
    pub checksum: String,
    /// The underlying base OSTree commit. Equals `checksum` when nothing is
    /// layered.
    pub base_checksum: String,
    /// Layered package NEVRA/name strings, verbatim from rpm-ostree.
    pub layered_packages: Vec<String>,
}

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("rpm-ostree not available on this host (not an atomic/rpm-ostree system?)")]
    NotAtomic,
    #[error("`rpm-ostree status --json` failed: {0}")]
    CommandFailed(String),
    #[error("could not parse rpm-ostree status json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("rpm-ostree status reported no booted deployment")]
    NoBootedDeployment,
    #[error(transparent)]
    Inspect(#[from] InspectError),
}

#[derive(Debug, Deserialize)]
struct StatusEnvelope {
    deployments: Vec<RawDeployment>,
}

#[derive(Debug, Deserialize)]
struct RawDeployment {
    #[serde(default)]
    booted: bool,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(rename = "base-checksum", default)]
    base_checksum: Option<String>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(rename = "requested-packages", default)]
    requested_packages: Vec<String>,
}

/// Parse `rpm-ostree status --json` output, extracting the booted
/// deployment. Pure — unit-testable against captured JSON with no
/// rpm-ostree present.
pub fn parse_status_json(json: &str) -> Result<DeploymentState, DeployError> {
    let env: StatusEnvelope = serde_json::from_str(json)?;
    let booted = env
        .deployments
        .into_iter()
        .find(|d| d.booted)
        .ok_or(DeployError::NoBootedDeployment)?;

    let checksum = booted
        .checksum
        .clone()
        .ok_or(DeployError::NoBootedDeployment)?;
    // `base-checksum` is only present when packages are layered on top;
    // otherwise the deployment checksum *is* the base commit.
    let base_checksum = booted.base_checksum.unwrap_or_else(|| checksum.clone());

    // Prefer the resolved `packages` list; fall back to requested if that's
    // all rpm-ostree surfaced.
    let layered_packages = if booted.packages.is_empty() {
        booted.requested_packages
    } else {
        booted.packages
    };

    Ok(DeploymentState {
        checksum,
        base_checksum,
        layered_packages,
    })
}

/// Probe the live host for its booted deployment. Returns
/// [`DeployError::NotAtomic`] (recoverable) when `rpm-ostree` is absent so a
/// caller on a non-atomic dev box can degrade gracefully rather than abort.
pub fn probe_deployment(sys: &dyn SystemInspector) -> Result<DeploymentState, DeployError> {
    if !sys.which("rpm-ostree") {
        return Err(DeployError::NotAtomic);
    }
    let out = sys.run("rpm-ostree", &["status", "--json"])?;
    if !out.success {
        return Err(DeployError::CommandFailed(out.stderr));
    }
    parse_status_json(&out.stdout)
}

impl DeploymentState {
    /// True if `pkg_name` is among the layered packages (prefix match on the
    /// NEVRA name component, so `firefox` matches `firefox-128.0-1.fc40`).
    pub fn has_layered_package(&self, pkg_name: &str) -> bool {
        self.layered_packages
            .iter()
            .any(|p| p == pkg_name || p.starts_with(&format!("{pkg_name}-")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockInspector;

    const LAYERED_JSON: &str = r#"{
      "deployments": [
        {
          "booted": true,
          "checksum": "aaa111deploychecksum",
          "base-checksum": "bbb222basecommit",
          "packages": ["opensnitch-1.6.0-1.fc40", "chkrootkit-0.58-1.fc40"],
          "requested-packages": ["opensnitch", "chkrootkit"]
        },
        {
          "booted": false,
          "checksum": "ccc333oldrollback"
        }
      ]
    }"#;

    const STOCK_JSON: &str = r#"{
      "deployments": [
        { "booted": true, "checksum": "stockcommit0000" }
      ]
    }"#;

    #[test]
    fn parses_layered_booted_deployment() {
        let d = parse_status_json(LAYERED_JSON).unwrap();
        assert_eq!(d.checksum, "aaa111deploychecksum");
        assert_eq!(d.base_checksum, "bbb222basecommit");
        assert_eq!(d.layered_packages.len(), 2);
        assert!(d.has_layered_package("opensnitch"));
        assert!(d.has_layered_package("chkrootkit"));
        assert!(!d.has_layered_package("firefox"));
    }

    #[test]
    fn stock_deployment_base_equals_checksum() {
        let d = parse_status_json(STOCK_JSON).unwrap();
        assert_eq!(d.checksum, "stockcommit0000");
        assert_eq!(d.base_checksum, "stockcommit0000");
        assert!(d.layered_packages.is_empty());
    }

    #[test]
    fn no_booted_deployment_errors() {
        let json = r#"{ "deployments": [ { "booted": false, "checksum": "x" } ] }"#;
        assert!(matches!(
            parse_status_json(json),
            Err(DeployError::NoBootedDeployment)
        ));
    }

    #[test]
    fn probe_without_rpm_ostree_is_not_atomic() {
        let sys = MockInspector::new();
        assert!(matches!(
            probe_deployment(&sys),
            Err(DeployError::NotAtomic)
        ));
    }

    #[test]
    fn probe_parses_through_the_inspector() {
        let sys = MockInspector::new().with_stdout("rpm-ostree", &["status", "--json"], STOCK_JSON);
        let d = probe_deployment(&sys).unwrap();
        assert_eq!(d.checksum, "stockcommit0000");
    }
}
