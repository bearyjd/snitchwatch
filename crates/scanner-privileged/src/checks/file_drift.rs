//! File-integrity drift (baseline spec §1 rules 1, 3, 4, 5 for rpm-owned
//! files), delegated to `rpm -Va` and classified with scanner-core's pure
//! tree. The privileged process never opens or hashes target files itself.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use scanner_core::deployment::DeploymentState;
use scanner_core::provenance::{under_any, DYNAMIC_ALLOWLIST_PREFIXES, MUTABLE_PREFIXES};
use scanner_core::rpmverify::{failing_entries, VerifyFailure};
use scanner_core::{classify, Classification, LayeredMatch, PathFacts};

use crate::checks::base_tree::{is_commit_hex, lookup_path_ok};
use crate::error::{Result, ScanError};
use crate::facts::SystemFacts;
use crate::model::{CheckType, Finding};

/// Whole-check time budget. Provisional (owner decision 2026-11-19, plan Q2):
/// to be revisited with real-host timing (plan Task 6).
pub const FILE_DRIFT_BUDGET: Duration = Duration::from_secs(180);

// Worst case: the file_drift budget, one candidate overshooting it with an
// `ostree ls`, an `ostree checksum` and an owner lookup, and a last slow
// primitive (chkrootkit) must all fit inside the scan watchdog.
const _: () = assert!(
    FILE_DRIFT_BUDGET.as_secs()
        + crate::facts::FILE_OWNER_DEADLINE.as_secs()
        + 2 * crate::facts::OSTREE_LOOKUP_DEADLINE.as_secs()
        + crate::command::PRIMITIVE_DEADLINE.as_secs()
        < crate::command::SCAN_WATCHDOG.as_secs()
);

/// Cap on listed failing paths; bounds memory and `rpm -qf` spawns.
pub const MAX_DRIFT_PATHS: usize = 500;

/// Result of the file-drift check.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FileDriftOutcome {
    pub findings: Vec<Finding>,
    /// Why the base-tree comparison did not run (every rpm failure is then
    /// reported, never fewer than a healthy run would report). `None` when it ran.
    pub base_tree_unavailable: Option<String>,
    /// `rpm -Va` lines that yielded no absolute path (never silently dropped).
    pub unparsed_lines: usize,
    /// Set when the run did not classify every candidate (over the cap, budget
    /// overrun, failed lookup). Findings so far are kept, but nothing may be
    /// resolved on the strength of an incomplete run.
    pub incomplete: Option<String>,
}

/// `deployment` is the one `rpm-ostree status --json` read of the scan.
pub fn run_file_drift(
    facts: &dyn SystemFacts,
    deployment: &DeploymentState,
) -> Result<FileDriftOutcome> {
    run_file_drift_within(facts, deployment, FILE_DRIFT_BUDGET)
}

/// [`run_file_drift`] with an explicit budget (a test seam). Every candidate
/// is classified first and only anomalous findings are capped, so noise cannot
/// crowd real anomalies out. A budget overrun or failed lookup keeps the
/// findings built so far and marks the outcome `incomplete`, never clean.
pub fn run_file_drift_within(
    facts: &dyn SystemFacts,
    deployment: &DeploymentState,
    budget: Duration,
) -> Result<FileDriftOutcome> {
    let started = Instant::now();
    let report = failing_entries(&facts.rpm_verify_all()?);
    let (commit, base_tree_unavailable) = base_tree_commit(facts, deployment);
    let mut anomalies = Vec::new();
    let mut incomplete = None;
    for failure in &report.failures {
        if started.elapsed() >= budget {
            incomplete = Some(format!(
                "file_drift budget of {}s exceeded",
                budget.as_secs()
            ));
            break;
        }
        match assess(facts, deployment, commit, failure) {
            Ok(Assessed {
                finding,
                lookup_failed,
            }) => {
                anomalies.extend(finding);
                if lookup_failed {
                    let path = escape_untrusted(&failure.path);
                    incomplete.get_or_insert(format!("{path}: base-tree lookup failed"));
                }
            }
            Err(e) => {
                incomplete = Some(escape_untrusted(&format!("{}: {e}", failure.path)));
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

/// The commit to compare against, or why the whole base-tree pass is
/// degraded (decided before the loop, so a run never degrades midway).
fn base_tree_commit<'a>(
    facts: &dyn SystemFacts,
    deployment: &'a DeploymentState,
) -> (Option<&'a str>, Option<String>) {
    if !facts.ostree_available() {
        return (None, Some("ostree is not installed".to_string()));
    }
    if !is_commit_hex(&deployment.checksum) {
        let why = "the booted deployment checksum is not a 64-hex commit";
        return (None, Some(why.to_string()));
    }
    (Some(deployment.checksum.as_str()), None)
}

/// One candidate's result. `lookup_failed` marks a decoy-shaped failure (a
/// deadline or I/O error on a path that still exists): the path is reported and
/// the run continues, but is incomplete so nothing resolves.
struct Assessed {
    finding: Option<Finding>,
    lookup_failed: bool,
}

impl Assessed {
    fn of(finding: Option<Finding>) -> Self {
        Assessed {
            finding,
            lookup_failed: false,
        }
    }
}

/// What the base commit says about one `rpm -Va` failure (rule 2).
enum BaseTree {
    /// Not compared (degraded pass, or a path that must not reach `ostree`).
    Unchecked,
    /// The deployed object is the committed one, or the image itself lacks a
    /// path rpm reports missing.
    Expected,
    Mismatch {
        expected: String,
        actual: String,
    },
    NotInCommit,
    MissingInCommit,
}

fn base_tree_verdict(
    facts: &dyn SystemFacts,
    commit: &str,
    failure: &VerifyFailure,
) -> Result<BaseTree> {
    let Some(entry) = facts.commit_entry(commit, &failure.path)? else {
        return Ok(if failure.missing {
            BaseTree::Expected
        } else {
            BaseTree::NotInCommit
        });
    };
    if failure.missing {
        return Ok(BaseTree::MissingInCommit);
    }
    let actual = facts.path_checksum(&failure.path)?;
    Ok(if actual == entry.checksum {
        BaseTree::Expected
    } else {
        BaseTree::Mismatch {
            expected: entry.checksum,
            actual,
        }
    })
}

/// Rule 1 (mutable location) and rule 4 (dynamic allowlist) vouch for a path
/// whatever its owner or checksum, so these paths cost no spawn at all.
fn vouched_without_spawn(path: &Path) -> bool {
    under_any(path, MUTABLE_PREFIXES) || under_any(path, DYNAMIC_ALLOWLIST_PREFIXES)
}

/// Run one `rpm -Va` failure through the classification tree; `Some` only
/// when anomalous. Rule 2 (the base-tree object comparison) runs before the
/// owner lookup, so image-baked files cost no `rpm -qf`. Owner lookups use the
/// raw path; findings use the escaped one.
fn assess(
    facts: &dyn SystemFacts,
    deployment: &DeploymentState,
    commit: Option<&str>,
    failure: &VerifyFailure,
) -> Result<Assessed> {
    let path = Path::new(&failure.path);
    if vouched_without_spawn(path) {
        return Ok(Assessed::of(None));
    }
    let base = match commit {
        Some(commit) if lookup_path_ok(&failure.path) => {
            match base_tree_verdict(facts, commit, failure) {
                Ok(verdict) => verdict,
                Err(e) => return lookup_failure(e, failure),
            }
        }
        Some(_) | None => BaseTree::Unchecked,
    };
    match &base {
        BaseTree::Expected => return Ok(Assessed::of(None)),
        BaseTree::MissingInCommit => {
            let detail = "missing (present in base commit)";
            let finding = drift_finding(Classification::Anomalous, &failure.path, detail);
            return Ok(Assessed::of(finding));
        }
        BaseTree::Unchecked | BaseTree::Mismatch { .. } | BaseTree::NotInCommit => {}
    }
    // A missing file has no on-disk owner to ask, so a missing layered file
    // reports plain "missing" (it is anomalous either way).
    let owner = if failure.missing {
        None
    } else {
        facts.file_owner(&failure.path)?
    };
    let layered = owner
        .as_deref()
        .is_some_and(|pkg| deployment.has_layered_package(pkg));
    let path_facts = PathFacts {
        path,
        in_mutable_location: false,
        base_tree_match: (owner.is_some() && !layered).then_some(false),
        layered_pkg: layered.then_some(LayeredMatch::Modified),
        on_dynamic_allowlist: false,
    };
    let detail = detail_for(failure, owner.as_deref(), layered, &base);
    let class = classify(&path_facts);
    Ok(Assessed::of(drift_finding(class, &failure.path, &detail)))
}

/// A vanished file (Q6) or an unavailable primitive stops the run as
/// incomplete. Any other failed lookup is reported as a finding so one decoy
/// (a FIFO, a device, a huge file) cannot hide the candidates after it.
fn lookup_failure(error: ScanError, failure: &VerifyFailure) -> Result<Assessed> {
    let reason = match &error {
        ScanError::Io { source, .. } => match source.kind() {
            io::ErrorKind::NotFound => return Err(error),
            _ if crate::command::is_watchdog_expired(source) => return Err(error),
            io::ErrorKind::TimedOut => "deadline exceeded",
            _ => "ostree failed",
        },
        ScanError::PrimitiveUnavailable(_) | ScanError::Sqlite(_) | ScanError::Poisoned => {
            return Err(error)
        }
    };
    let detail = format!("base-tree lookup failed: {reason}");
    Ok(Assessed {
        finding: drift_finding(Classification::Anomalous, &failure.path, &detail),
        lookup_failed: true,
    })
}

fn detail_for(
    failure: &VerifyFailure,
    owner: Option<&str>,
    layered: bool,
    base: &BaseTree,
) -> String {
    let who = match owner {
        Some(pkg) if layered => format!("layered_pkg:{pkg}"),
        Some(pkg) => format!("base_pkg:{pkg}"),
        None if failure.missing => return "missing".to_string(),
        None => "unowned".to_string(),
    };
    let attrs = &failure.attrs;
    match base {
        BaseTree::Mismatch { expected, actual } => {
            format!("base-tree mismatch [{attrs}] expected={expected} actual={actual} vs {who}")
        }
        BaseTree::NotInCommit => format!("not in base commit [{attrs}] vs {who}"),
        BaseTree::Unchecked | BaseTree::Expected | BaseTree::MissingInCommit if owner.is_some() => {
            format!("rpm -V mismatch [{attrs}] vs {who}")
        }
        BaseTree::Unchecked | BaseTree::Expected | BaseTree::MissingInCommit => who,
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
    use crate::checks::base_tree::{CommitEntry, EntryKind};
    use crate::facts::testing::SyntheticFacts;
    use crate::model::Classification;

    const COMMIT: &str = "d39c44ff4c9cf2ef204e6e025429558ac2772758b62e458a2a7d068fdd793410";

    fn deployment() -> DeploymentState {
        DeploymentState {
            checksum: COMMIT.to_string(),
            base_checksum: COMMIT.to_string(),
            layered_packages: vec!["opensnitch".to_string()],
        }
    }

    /// ostree-less host: the classification tests below that predate the
    /// base-tree pass exercise the degraded (rpm -Va only) path.
    fn facts(verify: &str) -> SyntheticFacts {
        healthy(verify).without_ostree()
    }

    fn healthy(verify: &str) -> SyntheticFacts {
        SyntheticFacts::new().with_rpm_verify(verify)
    }

    fn run(f: &SyntheticFacts) -> Result<FileDriftOutcome> {
        run_file_drift(f, &deployment())
    }

    fn paths(o: &FileDriftOutcome) -> Vec<&str> {
        o.findings.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn rule1_mutable_location_yields_no_finding() {
        let f = facts("S.5....T.    /var/log/x\n").with_owner("/var/log/x", "bash");
        assert!(run(&f).unwrap().findings.is_empty());
    }

    #[test]
    fn var_and_boot_rpm_paths_are_mutable_and_do_not_flood_the_cap() {
        // Stock ostree image: rpm -Va reports rpm-owned /var and /boot paths
        // as missing by design; they must not count toward the path cap.
        let mut verify =
            String::from("missing     /var/lib/rpm-state\nmissing     /boot/extlinux/cat.c32\n");
        for i in 0..(MAX_DRIFT_PATHS + 100) {
            verify.push_str(&format!("missing     /var/lib/flood/{i}\n"));
        }
        verify.push_str("..5......    /usr/bin/bash\n");
        let f = facts(&verify).with_owner("/usr/bin/bash", "bash");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/bash"]);
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn var_and_boot_prefix_lookalikes_are_still_assessed() {
        let f = facts("missing     /variable/x\nmissing     /bootloader/y\n");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/variable/x", "/bootloader/y"]);
    }

    #[test]
    fn rule3_layered_modified_is_anomalous_naming_the_package() {
        let f = facts("..5......    /usr/bin/opensnitchd\n")
            .with_owner("/usr/bin/opensnitchd", "opensnitch");
        let o = run(&f).unwrap();
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
        let o = run(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        assert!(o.findings[0].detail.contains("base_pkg:bash"));
    }

    #[test]
    fn rule4_etc_config_is_expected_no_finding() {
        let f = facts("S.5....T.  c /etc/foo.conf\n").with_owner("/etc/foo.conf", "foo");
        assert!(run(&f).unwrap().findings.is_empty());
    }

    #[test]
    fn missing_file_is_anomalous_with_missing_detail() {
        let f = facts("missing     /usr/bin/gone\n");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/gone"]);
        assert_eq!(o.findings[0].detail, "missing");
    }

    #[test]
    fn over_the_cap_lists_the_first_paths_plus_one_aggregate() {
        let n = MAX_DRIFT_PATHS + 1;
        let verify: String = (0..n)
            .map(|i| format!("..5......    /usr/lib/f{i}\n"))
            .collect();
        let o = run(&facts(&verify)).unwrap();
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
        let o = run(&facts(&verify)).unwrap();
        assert_eq!(o.findings.len(), MAX_DRIFT_PATHS);
        assert!(o.findings.iter().all(|f| f.path != "/"));
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn every_finding_path_is_absolute() {
        let f = facts("missing     /usr/bin/a\n..5......    /usr/bin/b\n");
        let o = run(&f).unwrap();
        assert_eq!(o.findings.len(), 2);
        assert!(o.findings.iter().all(|f| f.path.starts_with('/')));
    }

    #[test]
    fn control_characters_are_escaped_in_path_and_detail() {
        let verify = "..5......    /usr/bin/a\u{1b}[31mred\r\u{202e}x\n";
        let f = facts(verify).with_owner("/usr/bin/a\u{1b}[31mred\r\u{202e}x", "ev\u{1b}il");
        let o = run(&f).unwrap();
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
        let o = run(&facts("error: something odd\n")).unwrap();
        assert_eq!(o.unparsed_lines, 1);
        assert!(o.findings.is_empty());
    }

    #[test]
    fn an_exceeded_budget_keeps_findings_and_marks_the_run_incomplete() {
        let f = facts("..5......    /usr/bin/b\n").with_owner("/usr/bin/b", "bash");
        let o = run_file_drift_within(&f, &deployment(), Duration::ZERO).unwrap();
        assert!(o.findings.is_empty());
        assert!(o.incomplete.is_some_and(|r| r.contains("budget")));
    }

    #[test]
    fn a_failed_owner_lookup_keeps_earlier_findings_and_marks_incomplete() {
        let f = facts("..5......    /usr/bin/a\n..5......    /usr/bin/b\n")
            .with_owner_error("/usr/bin/b");
        let o = run(&f).unwrap();
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
        let o = run(&facts(&verify)).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a", "/usr/bin/b", "/usr/bin/c"]);
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn expected_paths_never_cost_an_owner_lookup() {
        // A lookup on this /etc path would fail; rule 4 needs no owner.
        let f = facts("..5......  c /etc/boom.conf\n..5......    /var/log/boom\n")
            .with_owner_error("/etc/boom.conf")
            .with_owner_error("/var/log/boom");
        let o = run(&f).unwrap();
        assert!(o.findings.is_empty());
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn unicode_line_and_zero_width_separators_are_escaped() {
        for c in [
            '\u{2028}', '\u{2029}', '\u{61c}', '\u{200b}', '\u{200c}', '\u{200d}',
        ] {
            let verify = format!("missing     /usr/bin/a{c}b\n");
            let o = run(&facts(&verify)).unwrap();
            let text = &o.findings[0].path;
            assert!(!text.contains(c), "{c:?} survived: {text:?}");
        }
    }

    const SUM_A: &str = "9b88d8cc9a6c591204bc4a12dd85488122834f756cc02f24ca3a1ddc5d49273f";
    const SUM_B: &str = "989b825aa7d0db2e775b8cd6f45719fb557ebb5916a93b6d963b2577bb0e6279";

    fn entry(sum: &str) -> CommitEntry {
        CommitEntry {
            kind: EntryKind::File,
            checksum: sum.to_string(),
        }
    }

    /// The commit holds `path` with checksum A and disk matches.
    fn matching(f: SyntheticFacts, path: &str) -> SyntheticFacts {
        f.with_commit_entry(path, entry(SUM_A))
            .with_path_checksum(path, SUM_A)
    }

    /// The commit holds `path` with checksum A and disk holds B.
    fn differing(f: SyntheticFacts, path: &str) -> SyntheticFacts {
        f.with_commit_entry(path, entry(SUM_A))
            .with_path_checksum(path, SUM_B)
    }

    #[test]
    fn rule2_equal_checksum_is_expected_with_no_owner_lookup() {
        // An owner lookup on this path would fail: rule 2 must come first.
        let f = matching(
            healthy("S.5....T.    /usr/lib/os-release\n"),
            "/usr/lib/os-release",
        )
        .with_owner_error("/usr/lib/os-release");
        let o = run(&f).unwrap();
        assert!(o.findings.is_empty());
        assert_eq!(o.incomplete, None);
        assert_eq!(o.base_tree_unavailable, None);
    }

    #[test]
    fn differing_checksum_is_anomalous_with_both_checksums_and_the_owner() {
        let f = differing(healthy("..5......    /usr/bin/bash\n"), "/usr/bin/bash")
            .with_owner("/usr/bin/bash", "bash");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/bash"]);
        let d = &o.findings[0].detail;
        assert!(d.contains(&format!("expected={SUM_A}")), "{d}");
        assert!(d.contains(&format!("actual={SUM_B}")), "{d}");
        assert!(d.contains("base_pkg:bash"), "{d}");
    }

    #[test]
    fn xattr_only_mismatch_stays_anomalous_and_unowned_is_named() {
        let f = differing(healthy("S.5....T.    /usr/etc/shells\n"), "/usr/etc/shells");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/etc/shells"]);
        assert!(o.findings[0].detail.contains("base-tree mismatch"));
        assert!(o.findings[0].detail.ends_with("unowned"));
    }

    #[test]
    fn a_path_absent_from_the_commit_is_anomalous() {
        let f = healthy("..5......    /usr/bin/implant\n")
            .with_commit_absent("/usr/bin/implant")
            .with_owner("/usr/bin/implant", "bash");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/implant"]);
        let d = &o.findings[0].detail;
        assert!(d.contains("not in base commit"), "{d}");
        assert!(d.contains("base_pkg:bash"), "{d}");
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn missing_on_disk_but_present_in_the_commit_is_anomalous() {
        let f =
            healthy("missing     /usr/bin/gone\n").with_commit_entry("/usr/bin/gone", entry(SUM_A));
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/gone"]);
        assert_eq!(o.findings[0].detail, "missing (present in base commit)");
    }

    #[test]
    fn missing_on_disk_and_absent_from_the_commit_is_expected() {
        let f = healthy("missing     /usr/local/bin\n").with_commit_absent("/usr/local/bin");
        let o = run(&f).unwrap();
        assert!(o.findings.is_empty());
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn layered_owned_differing_file_is_anomalous_naming_the_package() {
        let f = differing(
            healthy("..5......    /usr/bin/opensnitchd\n"),
            "/usr/bin/opensnitchd",
        )
        .with_owner("/usr/bin/opensnitchd", "opensnitch");
        let o = run(&f).unwrap();
        let d = &o.findings[0].detail;
        assert!(d.contains("layered_pkg:opensnitch"), "{d}");
        assert!(d.contains("base-tree mismatch"), "{d}");
    }

    #[test]
    fn mutable_and_allowlisted_paths_cost_no_ostree_spawn() {
        let f =
            healthy("..5......    /var/log/x\nmissing     /boot/x\n..5......  c /etc/foo.conf\n");
        let o = run(&f).unwrap();
        assert!(o.findings.is_empty());
        assert_eq!(o.incomplete, None);
        assert_eq!(f.lookups.get(), 0);
    }

    #[test]
    fn a_vanished_file_keeps_earlier_findings_marks_incomplete_and_stops() {
        let f = differing(
            healthy("..5......    /usr/bin/a\n..5......    /usr/bin/b\n..5......    /usr/bin/c\n"),
            "/usr/bin/a",
        )
        .with_commit_entry("/usr/bin/b", entry(SUM_A))
        .with_path_vanished("/usr/bin/b");
        let f = differing(f, "/usr/bin/c");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a"]);
        assert!(o.incomplete.is_some_and(|r| r.contains("/usr/bin/b")));
    }

    #[test]
    fn a_budget_overrun_during_lookups_keeps_findings_and_marks_incomplete() {
        let f = differing(
            healthy("..5......    /usr/bin/a\n..5......    /usr/bin/b\n"),
            "/usr/bin/a",
        );
        let f = differing(f, "/usr/bin/b").with_lookup_delay(Duration::from_millis(150));
        let o = run_file_drift_within(&f, &deployment(), Duration::from_millis(100)).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a"]);
        assert!(o.incomplete.is_some_and(|r| r.contains("budget")));
    }

    #[test]
    fn without_ostree_the_pass_is_degraded_with_no_spawn() {
        let f = healthy("S.5....T.    /usr/bin/bash\nmissing     /usr/bin/gone\n")
            .with_owner("/usr/bin/bash", "bash")
            .without_ostree();
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/bash", "/usr/bin/gone"]);
        assert!(o
            .base_tree_unavailable
            .is_some_and(|r| r.contains("ostree")));
        assert_eq!(o.incomplete, None);
        assert_eq!(f.lookups.get(), 0);
    }

    #[test]
    fn a_non_hex_deployment_checksum_degrades_the_pass_with_no_spawn() {
        let f = healthy("S.5....T.    /usr/bin/bash\n").with_owner("/usr/bin/bash", "bash");
        let mut d = deployment();
        d.checksum = "unknown".to_string();
        let o = run_file_drift(&f, &d).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/bash"]);
        assert!(o.base_tree_unavailable.is_some());
        assert_eq!(f.lookups.get(), 0);
    }

    #[test]
    fn degraded_findings_are_a_superset_of_healthy_findings() {
        let verify = "S.5....T.    /usr/bin/same\n..5......    /usr/bin/diff\n..5......    /usr/bin/new\n\
                      missing     /usr/bin/gone\nmissing     /usr/local/x\n..5......    /usr/lib/f\n";
        let build = |f: SyntheticFacts| {
            let f = matching(f, "/usr/bin/same");
            let f = differing(f, "/usr/bin/diff").with_commit_absent("/usr/bin/new");
            let f = f.with_commit_entry("/usr/bin/gone", entry(SUM_A));
            let f = f.with_commit_absent("/usr/local/x");
            matching(f, "/usr/lib/f")
        };
        let healthy_run = run(&build(healthy(verify))).unwrap();
        let degraded = run(&build(healthy(verify).without_ostree())).unwrap();
        let (h, d) = (paths(&healthy_run), paths(&degraded));
        assert_eq!(h, ["/usr/bin/diff", "/usr/bin/new", "/usr/bin/gone"]);
        assert!(h.iter().all(|p| d.contains(p)), "{h:?} not within {d:?}");
        assert!(d.len() > h.len());
    }

    #[test]
    fn an_unavailable_primitive_mid_run_is_incomplete_not_degraded() {
        let f = differing(
            healthy("..5......    /usr/bin/a\n..5......    /usr/bin/b\n..5......    /usr/bin/c\n"),
            "/usr/bin/a",
        );
        let f = differing(f, "/usr/bin/b");
        // /usr/bin/c has no commit_entry configured: PrimitiveUnavailable.
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a", "/usr/bin/b"]);
        assert!(o.incomplete.is_some_and(|r| r.contains("/usr/bin/c")));
        assert_eq!(o.base_tree_unavailable, None);
    }

    #[test]
    fn an_untouched_image_over_the_cap_yields_no_findings_and_a_complete_run() {
        let n = MAX_DRIFT_PATHS + 1;
        let verify: String = (0..n)
            .map(|i| format!("S.5....T.    /usr/lib64/f{i}.pyc\n"))
            .collect();
        let f = (0..n).fold(healthy(&verify), |f, i| {
            matching(f, &format!("/usr/lib64/f{i}.pyc"))
        });
        let o = run(&f).unwrap();
        assert!(o.findings.is_empty(), "{:?}", paths(&o));
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn a_path_with_control_characters_is_never_looked_up_and_stays_anomalous() {
        let raw = "/usr/bin/a\u{1b}[31m\rx";
        let f = healthy(&format!("..5......    {raw}\n")).with_owner(raw, "bash");
        let o = run(&f).unwrap();
        assert_eq!(o.findings.len(), 1);
        assert!(!o.findings[0].path.chars().any(|c| c.is_control()));
        assert_eq!(f.lookups.get(), 0);
        assert_eq!(o.incomplete, None);
    }

    #[test]
    fn a_decoy_whose_lookup_fails_is_reported_and_does_not_hide_later_candidates() {
        let verify = "..5......    /usr/bin/fifo\n..5......    /usr/bin/implant\n";
        let f = healthy(verify)
            .with_commit_entry("/usr/bin/fifo", entry(SUM_A))
            .with_path_timeout("/usr/bin/fifo");
        let f = differing(f, "/usr/bin/implant");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/fifo", "/usr/bin/implant"]);
        assert!(o.findings[0].detail.starts_with("base-tree lookup failed"));
        assert!(o.findings[1].detail.contains("base-tree mismatch"));
        assert!(o.incomplete.is_some_and(|r| r.contains("/usr/bin/fifo")));
    }

    #[test]
    fn a_generic_lookup_error_is_also_a_finding_and_the_run_continues() {
        let verify = "..5......    /usr/bin/a\n..5......    /usr/bin/b\n";
        let f = healthy(verify)
            .with_commit_entry("/usr/bin/a", entry(SUM_A))
            .with_path_checksum_error("/usr/bin/a");
        let f = differing(f, "/usr/bin/b");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a", "/usr/bin/b"]);
        assert!(o.incomplete.is_some());
    }

    #[test]
    fn a_failed_commit_lookup_is_a_finding_and_the_run_continues() {
        let verify = "missing     /usr/bin/a\n..5......    /usr/bin/b\n";
        let f = healthy(verify).with_commit_entry_error("/usr/bin/a");
        let f = differing(f, "/usr/bin/b");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a", "/usr/bin/b"]);
        assert!(o.incomplete.is_some());
    }

    #[test]
    fn an_expired_watchdog_stops_the_run_instead_of_adding_per_path_findings() {
        let verify = "..5......    /usr/bin/a\n..5......    /usr/bin/b\n..5......    /usr/bin/c\n";
        let f = differing(healthy(verify), "/usr/bin/a");
        let f = f
            .with_commit_entry("/usr/bin/b", entry(SUM_A))
            .with_path_watchdog_expired("/usr/bin/b");
        let f = differing(f, "/usr/bin/c");
        let o = run(&f).unwrap();
        assert_eq!(paths(&o), ["/usr/bin/a"]);
        assert!(o.incomplete.is_some_and(|r| r.contains("/usr/bin/b")));
    }

    #[test]
    fn the_vanished_stop_reason_escapes_the_path() {
        let raw = "/usr/bin/v\u{1b}[31m\u{202e}x";
        let f = healthy(&format!("..5......    {raw}\n")).with_owner_error(raw);
        let reason = run(&f).unwrap().incomplete.unwrap();
        assert!(
            !reason.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{reason:?}"
        );
        assert!(reason.contains("\\u{1b}"), "{reason}");
    }
}
