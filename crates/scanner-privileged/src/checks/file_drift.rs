//! File-integrity drift (baseline spec §1 rules 1, 3, 4, 5 for rpm-owned
//! files), delegated to `rpm -Va` and classified with scanner-core's pure
//! tree. The privileged process never opens or hashes target files itself.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use scanner_core::deployment::{parse_status_json, DeploymentState};
use scanner_core::provenance::{under_any, DYNAMIC_ALLOWLIST_PREFIXES, MUTABLE_PREFIXES};
use scanner_core::rpmverify::{failing_entries, VerifyFailure};
use scanner_core::{classify, Classification, LayeredMatch, PathFacts};

use crate::error::{Result, ScanError};
use crate::facts::SystemFacts;
use crate::model::{CheckType, Finding};

/// Whole-check time budget. Provisional (owner decision 2026-11-19, plan Q2):
/// to be revisited with real-host timing (plan Task 6).
pub const FILE_DRIFT_BUDGET: Duration = Duration::from_secs(180);

// Worst case: the file_drift budget, one owner lookup overshooting it, and a
// last slow primitive (chkrootkit) must all fit inside the scan watchdog.
const _: () = assert!(
    FILE_DRIFT_BUDGET.as_secs()
        + crate::facts::FILE_OWNER_DEADLINE.as_secs()
        + crate::command::PRIMITIVE_DEADLINE.as_secs()
        < crate::command::SCAN_WATCHDOG.as_secs()
);

/// Cap on listed failing paths; bounds memory and `rpm -qf` spawns.
pub const MAX_DRIFT_PATHS: usize = 500;

/// Result of the file-drift check.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FileDriftOutcome {
    pub findings: Vec<Finding>,
    /// Why the base-tree/unowned pass did not run (check is partial).
    pub base_tree_unavailable: Option<String>,
    /// `rpm -Va` lines that yielded no absolute path (never silently dropped).
    pub unparsed_lines: usize,
    /// Set when the run did not classify every candidate (over the cap, budget
    /// overrun, failed lookup). Findings so far are kept, but nothing may be
    /// resolved on the strength of an incomplete run.
    pub incomplete: Option<String>,
}

/// One path to classify: an `rpm -Va` failure, or a base-tree difference.
struct Candidate {
    path: String,
    /// `rpm -V` attribute field; `None` for base-tree candidates.
    attrs: Option<String>,
    missing: bool,
}

pub fn run_file_drift(facts: &dyn SystemFacts) -> Result<FileDriftOutcome> {
    run_file_drift_within(facts, FILE_DRIFT_BUDGET)
}

/// [`run_file_drift`] with an explicit budget (a test seam). Every candidate
/// is classified first and only anomalous findings are capped, so noise cannot
/// crowd real anomalies out. A budget overrun or failed lookup keeps the
/// findings built so far and marks the outcome `incomplete`, never clean.
pub fn run_file_drift_within(
    facts: &dyn SystemFacts,
    budget: Duration,
) -> Result<FileDriftOutcome> {
    let started = Instant::now();
    let deployment = read_deployment(facts)?;
    let report = failing_entries(&facts.rpm_verify_all()?);
    let (base_paths, base_tree_unavailable) = match facts.base_tree_drift() {
        Ok(paths) => (paths, None),
        Err(ScanError::PrimitiveUnavailable(reason)) => (Vec::new(), Some(reason)),
        Err(e) => return Err(e),
    };
    let mut anomalies = Vec::new();
    let mut incomplete = None;
    for candidate in candidates(&report.failures, base_paths) {
        if started.elapsed() >= budget {
            incomplete = Some(format!(
                "file_drift budget of {}s exceeded",
                budget.as_secs()
            ));
            break;
        }
        match assess(facts, &deployment, &candidate) {
            Ok(finding) => anomalies.extend(finding),
            Err(e) => {
                incomplete = Some(format!("{}: {e}", candidate.path));
                break;
            }
        }
    }
    let total = anomalies.len();
    if total > MAX_DRIFT_PATHS {
        anomalies.truncate(MAX_DRIFT_PATHS);
        anomalies.push(Finding::anomalous(
            CheckType::FileDrift,
            "/",
            format!("{total} paths failed rpm -Va; only the first {MAX_DRIFT_PATHS} are listed"),
        ));
        incomplete.get_or_insert_with(|| {
            format!("{total} anomalous paths; only the first {MAX_DRIFT_PATHS} are listed")
        });
    }
    Ok(FileDriftOutcome {
        findings: anomalies,
        base_tree_unavailable,
        unparsed_lines: report.unparsed,
        incomplete,
    })
}

fn read_deployment(facts: &dyn SystemFacts) -> Result<DeploymentState> {
    parse_status_json(&facts.deployment_status_json()?).map_err(|e| ScanError::Io {
        path: "rpm-ostree status --json".to_string(),
        source: io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
    })
}

/// rpm failures first, then base-tree paths not already listed by rpm.
fn candidates(failures: &[VerifyFailure], base_paths: Vec<String>) -> Vec<Candidate> {
    let seen: HashSet<&str> = failures.iter().map(|f| f.path.as_str()).collect();
    let from_base = base_paths
        .into_iter()
        .filter(|p| !seen.contains(p.as_str()))
        .map(|path| Candidate {
            path,
            attrs: None,
            missing: false,
        });
    failures
        .iter()
        .map(|f| Candidate {
            path: f.path.clone(),
            attrs: Some(f.attrs.clone()),
            missing: f.missing,
        })
        .chain(from_base)
        .collect()
}

/// Run one candidate through the classification tree; `Some` only when
/// anomalous. Owner lookups use the raw path; findings use the escaped one.
fn assess(
    facts: &dyn SystemFacts,
    deployment: &DeploymentState,
    candidate: &Candidate,
) -> Result<Option<Finding>> {
    let path = Path::new(&candidate.path);
    if under_any(path, MUTABLE_PREFIXES) {
        return Ok(None);
    }
    // Rule 4 vouches for the path whatever its owner: skip the `rpm -qf` spawn.
    if under_any(path, DYNAMIC_ALLOWLIST_PREFIXES) {
        return Ok(None);
    }
    // A missing file has no on-disk owner to ask, so a missing layered file
    // reports plain "missing" (it is anomalous either way).
    let owner = if candidate.missing {
        None
    } else {
        facts.file_owner(&candidate.path)?
    };
    let layered = owner
        .as_deref()
        .is_some_and(|pkg| deployment.has_layered_package(pkg));
    let path_facts = PathFacts {
        path,
        in_mutable_location: false,
        base_tree_match: (owner.is_some() && !layered).then_some(false),
        layered_pkg: layered.then_some(if candidate.attrs.is_some() {
            LayeredMatch::Modified
        } else {
            LayeredMatch::Clean
        }),
        on_dynamic_allowlist: false,
    };
    let detail = detail_for(candidate, owner.as_deref(), layered);
    Ok(drift_finding(
        classify(&path_facts),
        &candidate.path,
        &detail,
    ))
}

fn detail_for(candidate: &Candidate, owner: Option<&str>, layered: bool) -> String {
    let Some(pkg) = owner else {
        return if candidate.missing {
            "missing"
        } else {
            "unowned"
        }
        .to_string();
    };
    let kind = if layered { "layered_pkg" } else { "base_pkg" };
    match &candidate.attrs {
        Some(attrs) => format!("rpm -V mismatch [{attrs}] vs {kind}:{pkg}"),
        None => format!("differs from base commit vs {kind}:{pkg}"),
    }
}

/// Only an anomalous classification becomes a finding. Exhaustive on purpose:
/// a new scanner-core variant must fail to compile here.
fn drift_finding(class: Classification, path: &str, detail: &str) -> Option<Finding> {
    match class {
        Classification::Anomalous => Some(Finding::anomalous(
            CheckType::FileDrift,
            escape_untrusted(path),
            escape_untrusted(detail),
        )),
        Classification::NotApplicable | Classification::Expected => None,
    }
}

/// Escape control characters and bidi overrides (a file name is chosen by
/// whoever can write it) so reports cannot spoof a terminal or a reader.
fn escape_untrusted(text: &str) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut out, c| {
            match c {
                '\\' => out.push_str("\\\\"),
                c if c.is_control() || is_bidi_format(c) => {
                    out.push_str(&format!("\\u{{{:x}}}", c as u32));
                }
                c => out.push(c),
            }
            out
        })
}

fn is_bidi_format(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{61c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::testing::SyntheticFacts;
    use crate::model::Classification;

    const DEPLOY: &str =
        r#"{"deployments":[{"booted":true,"checksum":"abc","packages":["opensnitch"]}]}"#;

    fn facts(verify: &str) -> SyntheticFacts {
        SyntheticFacts::new()
            .with_deployment_json(DEPLOY)
            .with_rpm_verify(verify)
    }

    fn paths(o: &FileDriftOutcome) -> Vec<&str> {
        o.findings.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn rule1_mutable_location_yields_no_finding() {
        let f = facts("S.5....T.    /var/log/x\n").with_owner("/var/log/x", "bash");
        assert!(run_file_drift(&f).unwrap().findings.is_empty());
    }

    #[test]
    fn rule3_layered_clean_yields_no_finding() {
        // Layered file absent from rpm -Va output verifies clean; the only
        // way it is listed is a base-tree difference, which rule 3 vouches for.
        let f = facts("")
            .with_base_tree_drift(["/usr/bin/opensnitchd"])
            .with_owner("/usr/bin/opensnitchd", "opensnitch");
        assert!(run_file_drift(&f).unwrap().findings.is_empty());
    }

    #[test]
    fn rule3_layered_modified_is_anomalous_naming_the_package() {
        let f = facts("..5......    /usr/bin/opensnitchd\n")
            .with_owner("/usr/bin/opensnitchd", "opensnitch");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        let finding = &o.findings[0];
        assert_eq!(finding.check_type, CheckType::FileDrift);
        assert_eq!(finding.classification, Classification::Anomalous);
        assert_eq!(finding.path, "/usr/bin/opensnitchd");
        assert!(
            finding.detail.contains("layered_pkg:opensnitch"),
            "{finding:?}"
        );
        assert!(finding.detail.contains("[..5......]"), "{finding:?}");
    }

    #[test]
    fn base_package_failure_is_anomalous_naming_base_pkg() {
        let f = facts("S.5....T.    /usr/bin/bash\n").with_owner("/usr/bin/bash", "bash");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        assert!(o.findings[0].detail.contains("base_pkg:bash"));
    }

    #[test]
    fn rule4_etc_config_is_expected_no_finding() {
        let f = facts("S.5....T.  c /etc/foo.conf\n").with_owner("/etc/foo.conf", "foo");
        assert!(run_file_drift(&f).unwrap().findings.is_empty());
    }

    #[test]
    fn missing_file_is_anomalous_with_missing_detail() {
        let f = facts("missing     /usr/bin/gone\n");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/gone"]);
        assert_eq!(o.findings[0].detail, "missing");
    }

    #[test]
    fn rule5_unowned_under_usr_via_base_tree_drift_is_anomalous() {
        let f = facts("").with_base_tree_drift(["/usr/bin/implant"]);
        let o = run_file_drift(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/implant"]);
        assert!(o.findings[0].detail.contains("unowned"));
        assert_eq!(o.base_tree_unavailable, None);
    }

    #[test]
    fn unavailable_base_tree_marks_the_outcome_partial_but_keeps_findings() {
        let f = facts("S.5....T.    /usr/bin/bash\n").with_owner("/usr/bin/bash", "bash");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        assert!(o.base_tree_unavailable.is_some());
    }

    #[test]
    fn over_the_cap_lists_the_first_paths_plus_one_aggregate() {
        let n = MAX_DRIFT_PATHS + 1;
        let verify: String = (0..n)
            .map(|i| format!("..5......    /usr/lib/f{i}\n"))
            .collect();
        let o = run_file_drift(&facts(&verify)).unwrap();
        assert_eq!(o.findings.len(), MAX_DRIFT_PATHS + 1);
        let agg = o
            .findings
            .iter()
            .find(|f| f.path == "/")
            .expect("aggregate");
        assert!(agg.detail.contains(&n.to_string()), "{}", agg.detail);
        assert!(agg.detail.contains("first 500"), "{}", agg.detail);
        assert!(o.incomplete.is_some());
        assert!(!paths(&o).contains(&format!("/usr/lib/f{}", n - 1).as_str()));
    }

    #[test]
    fn at_the_cap_there_is_no_aggregate() {
        let verify: String = (0..MAX_DRIFT_PATHS)
            .map(|i| format!("..5......    /usr/lib/f{i}\n"))
            .collect();
        let o = run_file_drift(&facts(&verify)).unwrap();
        assert_eq!(o.findings.len(), MAX_DRIFT_PATHS);
        assert!(o.findings.iter().all(|f| f.path != "/"));
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn every_finding_path_is_absolute() {
        let f = facts("missing     /usr/bin/a\n..5......    /usr/bin/b\n")
            .with_base_tree_drift(["/usr/bin/c"]);
        let o = run_file_drift(&f).unwrap();
        assert_eq!(o.findings.len(), 3);
        assert!(o.findings.iter().all(|f| f.path.starts_with('/')));
    }

    #[test]
    fn control_characters_are_escaped_in_path_and_detail() {
        let verify = "..5......    /usr/bin/a\u{1b}[31mred\r\u{202e}x\n";
        let f = facts(verify).with_owner("/usr/bin/a\u{1b}[31mred\r\u{202e}x", "ev\u{1b}il");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        let finding = &o.findings[0];
        for text in [&finding.path, &finding.detail] {
            assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
            assert!(!text.contains('\u{202e}'), "{text:?}");
            assert!(text.contains("\\u{1b}"), "{text:?}");
        }
    }

    #[test]
    fn unparsed_rpm_lines_are_counted() {
        let o = run_file_drift(&facts("error: something odd\n")).unwrap();
        assert_eq!(o.unparsed_lines, 1);
        assert!(o.findings.is_empty());
    }

    #[test]
    fn an_exceeded_budget_keeps_findings_and_marks_the_run_incomplete() {
        let f = facts("..5......    /usr/bin/b\n").with_owner("/usr/bin/b", "bash");
        let o = run_file_drift_within(&f, Duration::ZERO).unwrap();
        assert!(o.findings.is_empty());
        assert!(o.incomplete.is_some_and(|r| r.contains("budget")));
    }

    #[test]
    fn a_failed_owner_lookup_keeps_earlier_findings_and_marks_incomplete() {
        let f = facts("..5......    /usr/bin/a\n..5......    /usr/bin/b\n")
            .with_owner_error("/usr/bin/b");
        let o = run_file_drift(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a"]);
        assert!(o.incomplete.is_some_and(|r| r.contains("/usr/bin/b")));
    }

    #[test]
    fn noise_does_not_crowd_real_anomalies_out_of_the_cap() {
        let verify: String = (0..600)
            .map(|i| format!("..5......    /var/log/n{i}\n..5......  c /etc/n{i}.conf\n"))
            .chain(
                [
                    "..5......    /usr/bin/a\n",
                    "..5......    /usr/bin/b\n",
                    "..5......    /usr/bin/c\n",
                ]
                .map(String::from),
            )
            .collect();
        let o = run_file_drift(&facts(&verify)).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a", "/usr/bin/b", "/usr/bin/c"]);
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn expected_paths_never_cost_an_owner_lookup() {
        // A lookup on this /etc path would fail; rule 4 needs no owner.
        let f = facts("..5......  c /etc/boom.conf\n..5......    /var/log/boom\n")
            .with_owner_error("/etc/boom.conf")
            .with_owner_error("/var/log/boom");
        let o = run_file_drift(&f).unwrap();
        assert!(o.findings.is_empty());
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn unicode_line_and_zero_width_separators_are_escaped() {
        for c in [
            '\u{2028}', '\u{2029}', '\u{61c}', '\u{200b}', '\u{200c}', '\u{200d}',
        ] {
            let verify = format!("missing     /usr/bin/a{c}b\n");
            let o = run_file_drift(&facts(&verify)).unwrap();
            let text = &o.findings[0].path;
            assert!(!text.contains(c), "{c:?} survived: {text:?}");
        }
    }
}
