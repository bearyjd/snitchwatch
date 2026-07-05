//! Assemble [`PathFacts`] from live reference sources and run the
//! classification tree — the orchestration glue between [`crate::inspector`]
//! and the pure [`crate::classify`] logic.
//!
//! ## Userspace-tier scope note
//!
//! Baseline design §1 rule 2 defines the authoritative base-tree check as a
//! direct OSTree content-addressed object-store lookup. That is a
//! privilege-justifying, expensive operation and belongs to the privileged
//! tier (Phase 6). In the **userspace** tier we use `rpm -qf` + `rpm -V` as
//! a cheap metadata proxy for file provenance/integrity: `rpm` reads each
//! package's own recorded manifest without elevation. Where the owning
//! package is a base-image package we treat a clean `rpm -V` as the
//! base-tree match; the privileged tier later confirms with the real OSTree
//! object lookup. This is called out explicitly rather than pretending the
//! userspace tier performs the full §1-rule-2 check.

use std::path::Path;

use crate::classify::{classify, Classification, LayeredMatch, PathFacts};
use crate::deployment::DeploymentState;
use crate::inspector::SystemInspector;
use crate::rpmverify::layered_match_for;

/// Prefixes considered inherently mutable / out of scan scope (rule 1).
pub const MUTABLE_PREFIXES: &[&str] =
    &["/var/log", "/home", "/tmp", "/run", "/proc", "/sys", "/dev"];

/// Curated dynamic/generated allowlist prefixes (rule 4). Intentionally
/// conservative; extend as real Bazzite testing surfaces false positives.
pub const DYNAMIC_ALLOWLIST_PREFIXES: &[&str] = &[
    "/etc",                   // ostree 3-way-merge user-modifiable tree
    "/usr/lib/tmpfiles.d",    // tmpfiles-declared
    "/usr/lib/sysusers.d",    // sysusers-declared
    "/run/systemd/generator", // systemd-generated units
    "/run/systemd/generator.late",
];

/// Result of classifying a path against live reference sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// The classification tree reached a verdict.
    Classified(Classification),
    /// Required tooling (`rpm`) was absent, so provenance could not be
    /// determined at the userspace tier — defer to the privileged tier.
    Undetermined,
}

/// True if `path` is under any of `prefixes`.
fn under_any(path: &Path, prefixes: &[&str]) -> bool {
    let s = path.to_string_lossy();
    prefixes
        .iter()
        .any(|p| s == *p || s.starts_with(&format!("{p}/")))
}

/// Owning package name for a path via `rpm -qf`, or `None` if unowned /
/// rpm unavailable.
fn owning_package(sys: &dyn SystemInspector, path: &Path) -> Option<String> {
    if !sys.which("rpm") {
        return None;
    }
    let out = sys
        .run(
            "rpm",
            &[
                "-qf",
                "--queryformat",
                "%{NAME}\\n",
                &path.to_string_lossy(),
            ],
        )
        .ok()?;
    if !out.success {
        // rpm exits non-zero with "file ... is not owned by any package".
        return None;
    }
    let name = out.stdout.trim();
    if name.is_empty() || name.contains("not owned") {
        None
    } else {
        // Guard against multi-owner output; take the first line.
        name.lines().next().map(|l| l.trim().to_string())
    }
}

/// Run one path through the tree using live reference sources.
pub fn classify_path(
    sys: &dyn SystemInspector,
    deployment: Option<&DeploymentState>,
    path: &Path,
) -> Provenance {
    // Rule 1 — cheap, no tooling needed.
    if under_any(path, MUTABLE_PREFIXES) {
        return Provenance::Classified(Classification::NotApplicable);
    }

    let on_dynamic_allowlist = under_any(path, DYNAMIC_ALLOWLIST_PREFIXES);

    // Without rpm we cannot resolve ownership/integrity at this tier.
    if !sys.which("rpm") {
        // A dynamic-allowlist path is still trustable by rule 4 alone.
        if on_dynamic_allowlist {
            return Provenance::Classified(Classification::Expected);
        }
        return Provenance::Undetermined;
    }

    let owner = owning_package(sys, path);
    let (base_tree_match, layered_pkg) = match owner {
        None => (None, None),
        Some(pkg) => {
            let verify = sys
                .run("rpm", &["-V", &pkg])
                .map(|o| o.stdout)
                .unwrap_or_default();
            let m = layered_match_for(&verify, path);
            let is_layered = deployment.is_some_and(|d| d.has_layered_package(&pkg));
            if is_layered {
                (None, Some(m))
            } else {
                // Base-image package: clean rpm -V is the userspace proxy for
                // the base-tree match (see module docs).
                let clean = m == LayeredMatch::Clean;
                (Some(clean), None)
            }
        }
    };

    let facts = PathFacts {
        path,
        in_mutable_location: false,
        base_tree_match,
        layered_pkg,
        on_dynamic_allowlist,
    };
    Provenance::Classified(classify(&facts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MockInspector;
    use std::path::PathBuf;

    fn deployment_with(layered: &[&str]) -> DeploymentState {
        DeploymentState {
            checksum: "dep".into(),
            base_checksum: "base".into(),
            layered_packages: layered.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn mutable_path_is_not_applicable_without_any_tooling() {
        let sys = MockInspector::new();
        let p = PathBuf::from("/home/user/.bashrc");
        assert_eq!(
            classify_path(&sys, None, &p),
            Provenance::Classified(Classification::NotApplicable)
        );
    }

    #[test]
    fn no_rpm_is_undetermined_for_immutable_path() {
        let sys = MockInspector::new();
        let p = PathBuf::from("/usr/bin/mystery");
        assert_eq!(classify_path(&sys, None, &p), Provenance::Undetermined);
    }

    #[test]
    fn no_rpm_but_dynamic_allowlist_is_expected() {
        let sys = MockInspector::new();
        let p = PathBuf::from("/etc/hosts");
        assert_eq!(
            classify_path(&sys, None, &p),
            Provenance::Classified(Classification::Expected)
        );
    }

    #[test]
    fn unowned_immutable_file_is_anomalous() {
        // rpm present, but -qf fails (not owned) -> no owner -> rule 5.
        let sys = MockInspector::new().with_command(
            "rpm",
            &["-qf", "--queryformat", "%{NAME}\\n", "/usr/bin/dropper"],
            crate::inspector::CommandOutput {
                stdout: "file /usr/bin/dropper is not owned by any package\n".into(),
                stderr: String::new(),
                success: false,
                code: Some(1),
            },
        );
        let p = PathBuf::from("/usr/bin/dropper");
        assert_eq!(
            classify_path(&sys, None, &p),
            Provenance::Classified(Classification::Anomalous)
        );
    }

    #[test]
    fn layered_package_clean_file_is_expected() {
        let p = "/usr/bin/opensnitchd";
        let sys = MockInspector::new()
            .with_stdout(
                "rpm",
                &["-qf", "--queryformat", "%{NAME}\\n", p],
                "opensnitch\n",
            )
            .with_stdout("rpm", &["-V", "opensnitch"], ""); // clean
        let dep = deployment_with(&["opensnitch-1.6.0-1.fc40"]);
        assert_eq!(
            classify_path(&sys, Some(&dep), &PathBuf::from(p)),
            Provenance::Classified(Classification::Expected)
        );
    }

    #[test]
    fn layered_package_tampered_file_is_anomalous() {
        let p = "/usr/bin/opensnitchd";
        let sys = MockInspector::new()
            .with_stdout(
                "rpm",
                &["-qf", "--queryformat", "%{NAME}\\n", p],
                "opensnitch\n",
            )
            .with_stdout(
                "rpm",
                &["-V", "opensnitch"],
                "S.5....T.    /usr/bin/opensnitchd\n",
            );
        let dep = deployment_with(&["opensnitch-1.6.0-1.fc40"]);
        assert_eq!(
            classify_path(&sys, Some(&dep), &PathBuf::from(p)),
            Provenance::Classified(Classification::Anomalous)
        );
    }

    #[test]
    fn base_package_clean_file_is_expected() {
        let p = "/usr/bin/bash";
        let sys = MockInspector::new()
            .with_stdout("rpm", &["-qf", "--queryformat", "%{NAME}\\n", p], "bash\n")
            .with_stdout("rpm", &["-V", "bash"], "");
        // bash is NOT in the layered set -> base package path.
        let dep = deployment_with(&["opensnitch"]);
        assert_eq!(
            classify_path(&sys, Some(&dep), &PathBuf::from(p)),
            Provenance::Classified(Classification::Expected)
        );
    }
}
