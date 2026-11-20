//! End-to-end integration test of the userspace tier's public API: drive a
//! multi-run scan lifecycle through the real `ScanStore` (on-disk SQLite via
//! a tempfile) with a `MockInspector` standing in for rpm-ostree/flatpak/ss,
//! and assert the new / still-outstanding / resolved diff behaves per
//! baseline design §4.

use std::path::PathBuf;

use scanner_core::checks::{shell_rc::ShellRcCheck, systemd_user::SystemdUserCheck};
use scanner_core::deployment::parse_status_json;
use scanner_core::provenance::{classify_path, Provenance};
use scanner_core::stores::scans::ScanStore;
use scanner_core::testkit::MockInspector;
use scanner_core::Classification;
use scanner_core::Scanner;

const STATUS_JSON: &str = r#"{
  "deployments": [
    { "booted": true, "checksum": "deadbeefcafe", "base-checksum": "basecommit",
      "packages": ["opensnitch-1.6.0-1.fc40"] }
  ]
}"#;

fn home() -> PathBuf {
    PathBuf::from("/home/analyst")
}

#[test]
fn scan_lifecycle_new_outstanding_resolved() {
    let tmp = tempfile::tempdir().unwrap();
    let scans = ScanStore::open(&tmp.path().join("scans.db")).unwrap();
    let deployment = Some(parse_status_json(STATUS_JSON).unwrap());

    // A curated check set so the assertions are deterministic regardless of
    // which host tools happen to exist in CI.
    let checks_first = || -> Vec<Box<dyn scanner_core::checks::Check>> {
        vec![Box::new(SystemdUserCheck), Box::new(ShellRcCheck)]
    };

    // Run 1 — baseline capture. One autostart entry exists and one rc file.
    let autostart = "/home/analyst/.config/autostart/keepassxc.desktop";
    let sys1 = MockInspector::new()
        .with_dir(
            "/home/analyst/.config/autostart",
            vec![PathBuf::from(autostart)],
        )
        .with_file(autostart, "[Desktop Entry]\nExec=keepassxc\n")
        .with_file("/home/analyst/.bashrc", "export EDITOR=vim\n");
    let r1 = Scanner::new(&sys1, &scans, checks_first())
        .run(home(), deployment.clone())
        .unwrap();
    assert!(r1.is_clean(), "first run baselines, no findings: {r1:?}");
    assert_eq!(r1.deployment_checksum, "deadbeefcafe");

    // Run 2 — a malicious autostart entry appears -> NEW.
    let evil = "/home/analyst/.config/autostart/evil.desktop";
    let sys2 = MockInspector::new()
        .with_dir(
            "/home/analyst/.config/autostart",
            vec![PathBuf::from(autostart), PathBuf::from(evil)],
        )
        .with_file(autostart, "[Desktop Entry]\nExec=keepassxc\n")
        .with_file(evil, "[Desktop Entry]\nExec=/tmp/dropper\n")
        .with_file("/home/analyst/.bashrc", "export EDITOR=vim\n");
    let r2 = Scanner::new(&sys2, &scans, checks_first())
        .run(home(), deployment.clone())
        .unwrap();
    assert_eq!(r2.new.len(), 1, "evil autostart is new: {r2:?}");
    assert!(r2.new[0].path.ends_with("evil.desktop"));

    // Run 3 — still there -> STILL OUTSTANDING.
    let r3 = Scanner::new(&sys2, &scans, checks_first())
        .run(home(), deployment.clone())
        .unwrap();
    assert_eq!(r3.still_outstanding.len(), 1);
    assert!(r3.new.is_empty());

    // Run 4 — attacker cleans up (entry removed) -> RESOLVED.
    let sys4 = MockInspector::new()
        .with_dir(
            "/home/analyst/.config/autostart",
            vec![PathBuf::from(autostart)],
        )
        .with_file(autostart, "[Desktop Entry]\nExec=keepassxc\n")
        .with_file("/home/analyst/.bashrc", "export EDITOR=vim\n");
    let r4 = Scanner::new(&sys4, &scans, checks_first())
        .run(home(), deployment)
        .unwrap();
    assert_eq!(r4.resolved.len(), 1);
    assert!(r4.resolved[0].path.ends_with("evil.desktop"));
    assert!(r4.is_clean());
}

const AUTOSTART_DIR: &str = "/home/analyst/.config/autostart";
const EVIL: &str = "/home/analyst/.config/autostart/evil.desktop";

fn checks() -> Vec<Box<dyn scanner_core::checks::Check>> {
    vec![Box::new(SystemdUserCheck), Box::new(ShellRcCheck)]
}

fn host(with_evil: bool) -> MockInspector {
    let mut entries = Vec::new();
    let mut sys = MockInspector::new().with_file("/home/analyst/.bashrc", "export EDITOR=vim\n");
    if with_evil {
        entries.push(PathBuf::from(EVIL));
        sys = sys.with_file(EVIL, "[Desktop Entry]\nExec=/tmp/dropper\n");
    }
    sys.with_dir(AUTOSTART_DIR, entries)
}

#[test]
fn prior_state_survives_store_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("scans.db");
    let deployment = Some(parse_status_json(STATUS_JSON).unwrap());
    let run = |sys: &MockInspector| {
        // Fresh open per run, as a separate process would do.
        let scans = ScanStore::open(&db).unwrap();
        let report = Scanner::new(sys, &scans, checks())
            .run(home(), deployment.clone())
            .unwrap();
        report
    };

    assert!(run(&host(false)).is_clean());
    assert_eq!(run(&host(true)).new.len(), 1);

    let r3 = run(&host(true));
    assert_eq!(
        r3.still_outstanding.len(),
        1,
        "prior state read from disk: {r3:?}"
    );
    assert!(r3.new.is_empty());

    let r4 = run(&host(false));
    assert_eq!(r4.resolved.len(), 1);
    assert!(r4.is_clean());
}

/// Baseline design §4: findings are keyed by `path`; the deployment checksum lives on
/// `scan_runs` only, so a deployment change must not reset or re-announce open findings.
#[test]
fn deployment_change_keeps_open_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let scans = ScanStore::open(&tmp.path().join("scans.db")).unwrap();
    let dep_a = Some(parse_status_json(STATUS_JSON).unwrap());
    let dep_b =
        Some(parse_status_json(&STATUS_JSON.replace("deadbeefcafe", "feedfacebabe")).unwrap());

    // The first run only baselines; the finding opens on the second run under checksum A.
    let r0 = Scanner::new(&host(false), &scans, checks())
        .run(home(), dep_a.clone())
        .unwrap();
    assert!(r0.is_clean());
    let r1 = Scanner::new(&host(true), &scans, checks())
        .run(home(), dep_a)
        .unwrap();
    assert_eq!(r1.new.len(), 1);
    assert_eq!(r1.deployment_checksum, "deadbeefcafe");

    let r2 = Scanner::new(&host(true), &scans, checks())
        .run(home(), dep_b)
        .unwrap();
    assert_eq!(r2.deployment_checksum, "feedfacebabe");
    assert_eq!(r2.still_outstanding.len(), 1, "{r2:?}");
    assert!(r2.new.is_empty());
}

fn rpm_host(path: &str, owner: &str, verify_out: &str) -> MockInspector {
    MockInspector::new()
        .with_stdout(
            "rpm",
            &["-qf", "--queryformat", "%{NAME}\\n", path],
            &format!("{owner}\n"),
        )
        .with_stdout("rpm", &["-V", owner], verify_out)
}

/// Cross-module seam: real `rpm-ostree status --json` parsing -> layered-package lookup ->
/// provenance classification.
#[test]
fn provenance_from_parsed_deployment() {
    let dep = parse_status_json(STATUS_JSON).unwrap();
    let layered = "/usr/bin/opensnitchd";
    let classify = |sys: &MockInspector, p: &str| classify_path(sys, Some(&dep), &PathBuf::from(p));

    assert_eq!(
        classify(&rpm_host(layered, "opensnitch", ""), layered),
        Provenance::Classified(Classification::Expected)
    );
    assert_eq!(
        classify(
            &rpm_host(layered, "opensnitch", "S.5....T.    /usr/bin/opensnitchd\n"),
            layered
        ),
        Provenance::Classified(Classification::Anomalous)
    );
    assert_eq!(
        classify(
            &MockInspector::new().with_stdout("rpm", &["--version"], "x"),
            "/etc/hosts"
        ),
        Provenance::Classified(Classification::Expected)
    );
}

/// A layered `python3-foo` must not make the base `python3` look layered.
#[test]
fn base_package_is_not_misattributed_to_layered_prefix_sibling() {
    let json = STATUS_JSON.replace("opensnitch-1.6.0-1.fc40", "python3-foo-1.0-1.fc40");
    let dep = parse_status_json(&json).unwrap();
    assert!(!dep.has_layered_package("python3"));
    assert!(dep.has_layered_package("python3-foo"));
}
