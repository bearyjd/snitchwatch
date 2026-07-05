//! End-to-end integration test of the userspace tier's public API: drive a
//! multi-run scan lifecycle through the real `ScanStore` (on-disk SQLite via
//! a tempfile) with a `MockInspector` standing in for rpm-ostree/flatpak/ss,
//! and assert the new / still-outstanding / resolved diff behaves per
//! baseline design §4.

use std::path::PathBuf;

use scanner_core::checks::{shell_rc::ShellRcCheck, systemd_user::SystemdUserCheck};
use scanner_core::deployment::parse_status_json;
use scanner_core::stores::scans::ScanStore;
use scanner_core::testkit::MockInspector;
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
