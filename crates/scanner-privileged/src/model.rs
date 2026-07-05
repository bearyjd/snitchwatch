//! Shared finding/report types for the privileged-tier scanner.
//!
//! These mirror the `findings` schema defined in the baseline design spec
//! (`docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` §4),
//! extended with the `check_type` discriminator column the privileged-tier
//! spec (§4) adds so Phase 5 (userspace) and Phase 6 (privileged) share one
//! schema and one diff/report code path.

use serde::{Deserialize, Serialize};

/// Which sub-check produced a finding. Serialized verbatim into the
/// `findings.check_type` column. String values are load-bearing — they are
/// the on-disk schema contract shared with Phase 5's userspace tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckType {
    FileDrift,
    KargsDrift,
    ModuleAnomaly,
    LockdownState,
    RootkitSignature,
}

impl CheckType {
    pub fn as_db(self) -> &'static str {
        match self {
            CheckType::FileDrift => "file_drift",
            CheckType::KargsDrift => "kargs_drift",
            CheckType::ModuleAnomaly => "module_anomaly",
            CheckType::LockdownState => "lockdown_state",
            CheckType::RootkitSignature => "rootkit_signature",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "file_drift" => CheckType::FileDrift,
            "kargs_drift" => CheckType::KargsDrift,
            "module_anomaly" => CheckType::ModuleAnomaly,
            "lockdown_state" => CheckType::LockdownState,
            "rootkit_signature" => CheckType::RootkitSignature,
            _ => return None,
        })
    }
}

/// Classification of a finding. The baseline spec stores only `anomalous`
/// rows; the privileged spec's Secure Boot/lockdown check adds an
/// `informational` current-state report that is NOT an anomaly unless a
/// state *transition* is detected. Only `Anomalous` findings are persisted
/// as reconciled `findings` rows; `Informational` ones are surfaced in the
/// report but not diffed across scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Classification {
    Anomalous,
    Informational,
}

impl Classification {
    pub fn as_db(self) -> &'static str {
        match self {
            Classification::Anomalous => "anomalous",
            Classification::Informational => "informational",
        }
    }
}

/// A single scanner finding.
///
/// `path` is reused (per the privileged spec §4) as a synthetic stable
/// identifier for non-filesystem findings — e.g. `kargs:mitigations=off`,
/// `module:xyz_hidden`, `lockdown:secureboot`,
/// `rootkit:chkrootkit:suckit` — so the new/still-outstanding/resolved diff
/// logic (which matches on `path` across scans) works identically regardless
/// of what the string represents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub check_type: CheckType,
    pub classification: Classification,
    /// Synthetic stable identifier (see struct docs).
    pub path: String,
    /// Human-readable detail. For rootkit findings this is chkrootkit's own
    /// output line, passed through verbatim (spec §4: don't reinterpret it).
    pub detail: String,
}

impl Finding {
    pub fn anomalous(
        check_type: CheckType,
        path: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            check_type,
            classification: Classification::Anomalous,
            path: path.into(),
            detail: detail.into(),
        }
    }

    pub fn informational(
        check_type: CheckType,
        path: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            check_type,
            classification: Classification::Informational,
            path: path.into(),
            detail: detail.into(),
        }
    }
}

/// Records that a sub-check could not run because a required system
/// primitive was unavailable (e.g. `chkrootkit` not installed,
/// `rpm-ostree` absent). Surfaced in the report so a degraded scan is
/// visibly degraded rather than silently reporting "all clear".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCheck {
    pub check: &'static str,
    pub reason: String,
}
