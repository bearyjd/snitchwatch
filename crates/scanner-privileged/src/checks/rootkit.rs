//! Rootkit signature scan — wraps `chkrootkit` as an external process.
//!
//! Per the privileged spec §1 we wrap **`chkrootkit`** (actively maintained,
//! current-threat signatures) rather than `rkhunter` (stale since 2018). We
//! parse its stable, line-oriented plain-text output — we do NOT reimplement
//! any detection logic, and we pass its detection text through verbatim
//! (spec §4: don't reformat/reinterpret it).
//!
//! `chkrootkit` may not be installed. Presence is checked first; when absent
//! the check is *skipped with a recorded note*, never silently treated as
//! "clean".

use crate::error::Result;
use crate::facts::SystemFacts;
use crate::model::{CheckType, Finding};

/// Outcome of the rootkit sub-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootkitOutcome {
    /// chkrootkit ran; these are the infected/warning findings (possibly empty).
    Scanned(Vec<Finding>),
    /// chkrootkit is not installed on this host; the check was skipped.
    Skipped { reason: String },
}

/// Parse chkrootkit's plain-text output into findings.
///
/// chkrootkit emits one line per test, e.g.:
///   `Checking 'aliens'... nothing found`
///   `Checking 'lkm'... INFECTED`
///   `Searching for suckit rootkit... Warning: /sbin/init INFECTED`
/// We flag lines containing `INFECTED` (its positive-detection marker) and
/// synthesize a stable `path` id from the checked test name where we can.
pub fn parse_chkrootkit_output(output: &str) -> Vec<Finding> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("INFECTED"))
        .map(|line| {
            let test = extract_test_name(line).unwrap_or("unknown");
            Finding::anomalous(
                CheckType::RootkitSignature,
                format!("rootkit:chkrootkit:{test}"),
                // Verbatim chkrootkit output line (spec §4).
                line.to_string(),
            )
        })
        .collect()
}

/// Best-effort extraction of the test/rootkit name from a chkrootkit line,
/// used only to build a stable `path` identifier. The human-readable detail
/// always carries the full verbatim line regardless.
fn extract_test_name(line: &str) -> Option<&str> {
    // Form: `Checking 'lkm'... INFECTED`
    if let Some(rest) = line.strip_prefix("Checking '") {
        if let Some(end) = rest.find('\'') {
            return Some(&rest[..end]);
        }
    }
    // Form: `Searching for suckit rootkit... Warning: ...`
    if let Some(rest) = line.strip_prefix("Searching for ") {
        return rest.split_whitespace().next();
    }
    None
}

/// Run the rootkit sub-check via the provided facts. Checks presence first;
/// if `chkrootkit` is absent, returns `Skipped` rather than erroring.
pub fn scan_rootkits(facts: &dyn SystemFacts) -> Result<RootkitOutcome> {
    match facts.chkrootkit_path() {
        None => Ok(RootkitOutcome::Skipped {
            reason: "chkrootkit not installed on this host".to_string(),
        }),
        Some(program) => {
            let output = facts.run_chkrootkit(&program)?;
            Ok(RootkitOutcome::Scanned(parse_chkrootkit_output(&output)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::testing::SyntheticFacts;

    #[test]
    fn clean_output_yields_no_findings() {
        let out = "\
Checking 'aliens'... nothing found
Checking 'lkm'... nothing found
";
        assert!(parse_chkrootkit_output(out).is_empty());
    }

    #[test]
    fn infected_line_is_flagged_verbatim() {
        let out = "Checking 'lkm'... INFECTED";
        let findings = parse_chkrootkit_output(out);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "rootkit:chkrootkit:lkm");
        assert_eq!(findings[0].detail, "Checking 'lkm'... INFECTED");
        assert_eq!(findings[0].check_type, CheckType::RootkitSignature);
    }

    #[test]
    fn searching_form_extracts_rootkit_name() {
        let out = "Searching for suckit rootkit... Warning: /sbin/init INFECTED";
        let findings = parse_chkrootkit_output(out);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "rootkit:chkrootkit:suckit");
    }

    #[test]
    fn absent_chkrootkit_is_skipped_not_clean() {
        let facts = SyntheticFacts::new(); // no chkrootkit configured
        match scan_rootkits(&facts).unwrap() {
            RootkitOutcome::Skipped { reason } => assert!(reason.contains("not installed")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn present_chkrootkit_runs_and_parses() {
        let facts = SyntheticFacts::new()
            .with_chkrootkit("/usr/sbin/chkrootkit", "Checking 'lkm'... INFECTED\n");
        match scan_rootkits(&facts).unwrap() {
            RootkitOutcome::Scanned(f) => assert_eq!(f.len(), 1),
            other => panic!("expected Scanned, got {other:?}"),
        }
    }
}
