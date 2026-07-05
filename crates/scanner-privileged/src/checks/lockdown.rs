//! Secure Boot / kernel-lockdown state check.
//!
//! Per the privileged spec §2, the conservative default (absent an explicit
//! user/deployment expectation, which is a stop-and-ask) is: report the
//! current state as **informational**, and only escalate to an **anomaly**
//! when the state has *transitioned to a weaker posture* since the last scan
//! (Secure Boot enabled→disabled, or lockdown active→none). A transition is
//! a far stronger signal than an absolute "off" state, which may simply
//! reflect the user's own hardware/firmware choice.
//!
//! Prior state is read from / written to the `state_snapshots` table so the
//! next scan can diff against it. This is the ONLY cross-scan state these
//! checks need — transitions are detected via snapshots, not via persisted
//! findings.

use crate::model::{CheckType, Finding};

pub const SNAP_SECURE_BOOT: &str = "secure_boot";
pub const SNAP_LOCKDOWN: &str = "lockdown";

/// Result of evaluating one state dimension: the value to persist as the new
/// snapshot, plus a finding to surface (informational, or anomalous on a
/// weakening transition).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateEval {
    pub snapshot_key: &'static str,
    pub new_value: String,
    pub finding: Finding,
}

/// Evaluate Secure Boot state against its prior snapshot.
/// `prior` is the value recorded at the last scan (`None` = first ever scan).
pub fn eval_secure_boot(prior: Option<&str>, current_enabled: bool) -> StateEval {
    let current = if current_enabled {
        "enabled"
    } else {
        "disabled"
    };
    let weakened = matches!(prior, Some("enabled")) && !current_enabled;

    let finding = if weakened {
        Finding::anomalous(
            CheckType::LockdownState,
            "lockdown:secureboot",
            "Secure Boot state changed: was enabled at last scan, now disabled",
        )
    } else {
        Finding::informational(
            CheckType::LockdownState,
            "lockdown:secureboot",
            format!("Secure Boot is currently {current}"),
        )
    };

    StateEval {
        snapshot_key: SNAP_SECURE_BOOT,
        new_value: current.to_string(),
        finding,
    }
}

/// Ordering of lockdown modes from weakest to strongest.
fn lockdown_rank(mode: &str) -> u8 {
    match mode {
        "none" => 0,
        "integrity" => 1,
        "confidentiality" => 2,
        _ => 0, // unknown token treated as weakest (conservative)
    }
}

/// Evaluate kernel-lockdown state against its prior snapshot. A move to a
/// weaker mode (e.g. integrity→none) is a weakening transition and flagged
/// as anomalous; anything else is informational.
pub fn eval_lockdown(prior: Option<&str>, current: &str) -> StateEval {
    let weakened = prior
        .map(|p| lockdown_rank(current) < lockdown_rank(p))
        .unwrap_or(false);

    let finding = if weakened {
        let prior = prior.unwrap_or("unknown");
        Finding::anomalous(
            CheckType::LockdownState,
            "lockdown:kernel",
            format!("kernel lockdown weakened: was '{prior}' at last scan, now '{current}'"),
        )
    } else {
        Finding::informational(
            CheckType::LockdownState,
            "lockdown:kernel",
            format!("kernel lockdown is currently '{current}'"),
        )
    };

    StateEval {
        snapshot_key: SNAP_LOCKDOWN,
        new_value: current.to_string(),
        finding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Classification;

    #[test]
    fn secure_boot_first_scan_is_informational() {
        let eval = eval_secure_boot(None, true);
        assert_eq!(eval.finding.classification, Classification::Informational);
        assert_eq!(eval.new_value, "enabled");
    }

    #[test]
    fn secure_boot_stays_off_is_informational_not_anomaly() {
        // Absolute "off" that was already off is NOT flagged (spec: only
        // transitions escalate; an always-off box may be the user's choice).
        let eval = eval_secure_boot(Some("disabled"), false);
        assert_eq!(eval.finding.classification, Classification::Informational);
    }

    #[test]
    fn secure_boot_enabled_to_disabled_is_anomalous() {
        let eval = eval_secure_boot(Some("enabled"), false);
        assert_eq!(eval.finding.classification, Classification::Anomalous);
        assert!(eval.finding.detail.contains("was enabled"));
    }

    #[test]
    fn secure_boot_disabled_to_enabled_is_not_anomalous() {
        // Strengthening posture is never an anomaly.
        let eval = eval_secure_boot(Some("disabled"), true);
        assert_eq!(eval.finding.classification, Classification::Informational);
    }

    #[test]
    fn lockdown_weakening_is_anomalous() {
        let eval = eval_lockdown(Some("integrity"), "none");
        assert_eq!(eval.finding.classification, Classification::Anomalous);
    }

    #[test]
    fn lockdown_strengthening_is_informational() {
        let eval = eval_lockdown(Some("none"), "confidentiality");
        assert_eq!(eval.finding.classification, Classification::Informational);
    }

    #[test]
    fn lockdown_unchanged_is_informational() {
        let eval = eval_lockdown(Some("none"), "none");
        assert_eq!(eval.finding.classification, Classification::Informational);
    }
}
