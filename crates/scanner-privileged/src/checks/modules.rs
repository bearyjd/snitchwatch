//! Loaded kernel-module audit.
//!
//! Enumerates `/proc/modules` and classifies each loaded module through the
//! baseline spec's own three-tier provenance model (privileged spec §2) —
//! NOT a bespoke hardware-dependent "expected modules" allowlist:
//!
//!   1. Shipped under the base OSTree tree's `/usr/lib/modules/$(uname -r)/`
//!      → expected (base tree).
//!   2. Shipped by a layered/DKMS package (`rpm -qf` resolves an owner)
//!      → expected (layered package).
//!   3. Neither → anomalous. Severity is informed by signature status: an
//!      unsigned module outside both tiers is exactly the signal a
//!      rootkit-installed module would produce.

use crate::error::Result;
use crate::facts::SystemFacts;
use crate::model::{CheckType, Finding};

/// Module provenance, mirroring the baseline spec's file-provenance tiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    BaseTree,
    LayeredPackage(String),
    Anomalous { signed: bool },
}

/// Parse the module names (first whitespace-delimited column) from
/// `/proc/modules` content.
pub fn parse_loaded_modules(proc_modules: &str) -> Vec<String> {
    proc_modules
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .collect()
}

/// Classify one module against the three tiers using the provided facts.
///
/// Kept as a standalone function taking `&dyn SystemFacts` + the precomputed
/// base-tree set so it is unit-testable against synthetic provenance data.
pub fn classify_module(
    module: &str,
    base_tree: &std::collections::HashSet<String>,
    facts: &dyn SystemFacts,
) -> Result<Provenance> {
    if base_tree.contains(module) {
        return Ok(Provenance::BaseTree);
    }
    if let Some(pkg) = facts.module_layered_package(module)? {
        return Ok(Provenance::LayeredPackage(pkg));
    }
    // Tier 3: anomalous. Signature presence informs severity/detail.
    // A signature-check failure is treated as "unsigned" (conservative:
    // we could not prove it signed) rather than aborting the whole audit.
    let signed = facts.module_signed(module).unwrap_or(false);
    Ok(Provenance::Anomalous { signed })
}

/// Audit all loaded modules, returning one finding per anomalous module.
/// Base-tree and layered-package modules produce no finding (expected).
pub fn audit_modules(facts: &dyn SystemFacts) -> Result<Vec<Finding>> {
    let proc_modules = facts.proc_modules()?;
    let base_tree: std::collections::HashSet<String> =
        facts.base_tree_modules()?.into_iter().collect();

    let mut findings = Vec::new();
    for module in parse_loaded_modules(&proc_modules) {
        match classify_module(&module, &base_tree, facts)? {
            Provenance::BaseTree | Provenance::LayeredPackage(_) => {}
            Provenance::Anomalous { signed } => {
                let sig_note = if signed {
                    "signed"
                } else {
                    "unsigned (modinfo sig_key empty)"
                };
                findings.push(Finding::anomalous(
                    CheckType::ModuleAnomaly,
                    format!("module:{module}"),
                    format!(
                        "loaded module '{module}' not found in base tree \
                         /usr/lib/modules nor any layered package; {sig_note}"
                    ),
                ));
            }
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::testing::SyntheticFacts;

    #[test]
    fn parses_proc_modules_names() {
        let content = "\
xfs 2916352 1 - Live 0xffffffffc0a00000
nvidia 62640128 456 - Live 0xffffffffc0400000 (POE)
";
        assert_eq!(parse_loaded_modules(content), vec!["xfs", "nvidia"]);
    }

    #[test]
    fn base_tree_module_is_expected() {
        let facts = SyntheticFacts::new().with_base_tree_modules(["xfs"]);
        let base: std::collections::HashSet<String> = ["xfs".to_string()].into_iter().collect();
        assert_eq!(
            classify_module("xfs", &base, &facts).unwrap(),
            Provenance::BaseTree
        );
    }

    #[test]
    fn layered_package_module_is_expected() {
        let facts = SyntheticFacts::new().with_layered_module("nvidia", "akmod-nvidia-3:550.el9");
        let base = std::collections::HashSet::new();
        assert_eq!(
            classify_module("nvidia", &base, &facts).unwrap(),
            Provenance::LayeredPackage("akmod-nvidia-3:550.el9".to_string())
        );
    }

    #[test]
    fn unknown_unsigned_module_is_anomalous() {
        let facts = SyntheticFacts::new(); // no base tree, no layered owner, unsigned
        let base = std::collections::HashSet::new();
        assert_eq!(
            classify_module("xyz_hidden", &base, &facts).unwrap(),
            Provenance::Anomalous { signed: false }
        );
    }

    #[test]
    fn audit_flags_only_the_anomalous_module() {
        let facts = SyntheticFacts::new()
            .with_proc_modules(
                "xfs 1 0 - Live 0x0\nnvidia 1 0 - Live 0x0\nxyz_hidden 1 0 - Live 0x0\n",
            )
            .with_base_tree_modules(["xfs"])
            .with_layered_module("nvidia", "akmod-nvidia");
        let findings = audit_modules(&facts).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "module:xyz_hidden");
        assert!(findings[0].detail.contains("unsigned"));
    }

    #[test]
    fn signed_unknown_module_notes_signature() {
        let facts = SyntheticFacts::new()
            .with_proc_modules("weird 1 0 - Live 0x0\n")
            .with_signed_module("weird");
        let findings = audit_modules(&facts).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].detail.contains("signed"));
        assert!(!findings[0].detail.contains("unsigned"));
    }
}
