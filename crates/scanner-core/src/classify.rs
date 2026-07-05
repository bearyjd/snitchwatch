//! The expected-drift-vs-anomaly classification tree.
//!
//! Direct implementation of baseline design §1. Every changed/flagged path
//! is run through an ordered five-step decision tree; the first matching
//! rule decides, with a strict fallthrough to `Anomalous` and no ambiguous
//! middle category.
//!
//! This module is **pure**: it operates on [`PathFacts`] that a caller
//! assembles from the reference sources (rpm-ostree status, `rpm -V`, the
//! OSTree object store, the dynamic-path allowlist). Keeping the tree pure
//! is what makes the hard design logic unit-testable without shelling out.

use std::path::Path;

/// Outcome of running one path through the classification tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Rule 1 — under an inherently mutable location; not a finding either
    /// way.
    NotApplicable,
    /// Rules 2–4 — accounted for by a trusted reference source.
    Expected,
    /// Rule 5 — strict fallthrough; nothing trusted vouches for this path.
    Anomalous,
}

/// Result of matching a path against its owning layered package's own
/// shipped manifest (i.e. what `rpm -V <pkg>` reports for that path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayeredMatch {
    /// Path is owned by a layered package and matches its recorded
    /// checksum/mode/owner (`rpm -V` reports no diff). Rule 3 → expected.
    Clean,
    /// Path is owned by a layered package but does **not** match its own
    /// shipped manifest. Per baseline design §3 this is anomalous
    /// *regardless* of the package being "allowlisted" — a compromised
    /// layered package tampering with its own files must still be caught.
    Modified,
}

/// The reference-source facts about a single path, assembled by the caller
/// from rpm-ostree / rpm / OSTree / the dynamic allowlist. Mirrors the
/// inputs each rule of baseline design §1 consults.
#[derive(Debug, Clone)]
pub struct PathFacts<'a> {
    pub path: &'a Path,
    /// Rule 1: the path is under an inherently mutable, never-scanned
    /// location (`/var/log`, `/home`, `/tmp`, `/run`, user data).
    pub in_mutable_location: bool,
    /// Rule 2: `Some(true)` — owned by the active deployment's base OSTree
    /// commit and current content matches the object the commit records;
    /// `Some(false)` — owned by the base tree but content diverges;
    /// `None` — not owned by the base tree.
    pub base_tree_match: Option<bool>,
    /// Rule 3: match state against the owning layered package's own
    /// manifest, or `None` if no layered package owns the path.
    pub layered_pkg: Option<LayeredMatch>,
    /// Rule 4: the path is on the curated dynamic/generated allowlist
    /// (tmpfiles.d/sysusers.d/systemd-generated/`ostree admin config-diff`
    /// user-modifiable `/etc`). Trusted regardless of checksum.
    pub on_dynamic_allowlist: bool,
}

impl<'a> PathFacts<'a> {
    /// A path with no trusted vouching source at all — the common shape for
    /// "an unowned file appeared in a nominally immutable location".
    pub fn unowned(path: &'a Path) -> Self {
        Self {
            path,
            in_mutable_location: false,
            base_tree_match: None,
            layered_pkg: None,
            on_dynamic_allowlist: false,
        }
    }
}

/// Run a path's facts through the ordered five-step tree (baseline design
/// §1). The first matching rule wins.
pub fn classify(facts: &PathFacts<'_>) -> Classification {
    // Rule 1 — out of scan scope entirely.
    if facts.in_mutable_location {
        return Classification::NotApplicable;
    }
    // Rule 2 — base tree owns it and the checksum matches.
    if facts.base_tree_match == Some(true) {
        return Classification::Expected;
    }
    // Rule 3 — layered package owns it and it matches that RPM's manifest.
    // A layered `Modified` deliberately does NOT return here: it falls
    // through so that only the dynamic allowlist (rule 4) can still rescue
    // it; otherwise it lands on rule 5 (anomalous), per §3's threat model.
    if facts.layered_pkg == Some(LayeredMatch::Clean) {
        return Classification::Expected;
    }
    // Rule 4 — curated dynamic/generated path, trusted regardless of
    // checksum. This is what absorbs the /etc 3-way-merge and
    // tmpfiles/sysusers cases that legitimately diverge from any static
    // manifest.
    if facts.on_dynamic_allowlist {
        return Classification::Expected;
    }
    // Rule 5 — strict fallthrough.
    Classification::Anomalous
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn rule1_mutable_location_is_not_applicable() {
        let path = p("/home/user/.cache/thing");
        let facts = PathFacts {
            in_mutable_location: true,
            // Even with a base mismatch, rule 1 short-circuits first.
            base_tree_match: Some(false),
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::NotApplicable);
    }

    #[test]
    fn rule2_base_tree_checksum_match_is_expected() {
        let path = p("/usr/bin/bash");
        let facts = PathFacts {
            base_tree_match: Some(true),
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Expected);
    }

    #[test]
    fn rule2_base_tree_checksum_mismatch_falls_through_to_anomalous() {
        let path = p("/usr/bin/bash");
        let facts = PathFacts {
            base_tree_match: Some(false),
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Anomalous);
    }

    #[test]
    fn rule3_layered_clean_is_expected() {
        let path = p("/usr/bin/firefox");
        let facts = PathFacts {
            layered_pkg: Some(LayeredMatch::Clean),
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Expected);
    }

    #[test]
    fn rule3_layered_modified_is_anomalous_even_though_pkg_is_present() {
        // §3 threat model: a layered package tampering with its own files
        // after install must still be caught — presence never becomes a
        // blanket exemption.
        let path = p("/usr/bin/firefox");
        let facts = PathFacts {
            layered_pkg: Some(LayeredMatch::Modified),
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Anomalous);
    }

    #[test]
    fn rule4_dynamic_allowlist_rescues_a_layered_modified_etc_file() {
        // A user-modified /etc file: base tree records a different default,
        // rpm -V would report it modified, but ostree's 3-way merge marks
        // it user-modifiable — allowlist makes it expected.
        let path = p("/etc/ssh/sshd_config");
        let facts = PathFacts {
            base_tree_match: Some(false),
            layered_pkg: Some(LayeredMatch::Modified),
            on_dynamic_allowlist: true,
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Expected);
    }

    #[test]
    fn rule5_unowned_file_in_immutable_location_is_anomalous() {
        let path = p("/usr/bin/totally-new-binary");
        assert_eq!(
            classify(&PathFacts::unowned(&path)),
            Classification::Anomalous
        );
    }

    #[test]
    fn ordering_base_match_wins_over_allowlist_absence() {
        // Rule 2 fires before rule 4 is even consulted.
        let path = p("/usr/lib/systemd/system/foo.service");
        let facts = PathFacts {
            base_tree_match: Some(true),
            on_dynamic_allowlist: false,
            ..PathFacts::unowned(&path)
        };
        assert_eq!(classify(&facts), Classification::Expected);
    }
}
