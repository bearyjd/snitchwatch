//! Producer side of the scanner report contract. The broker and the Kirigami
//! GUI consume `fixtures/report.json`; this pins it to the real producer.

use scanner_privileged::scan::report_json;
use scanner_privileged::store::{DiffStatus, ReconciledFinding};
use scanner_privileged::{
    CheckType, Classification, Finding, ReconcileReport, ScanReport, SkippedCheck,
};

fn rf(path: &str, status: DiffStatus, detail: &str) -> ReconciledFinding {
    ReconciledFinding {
        path: path.into(),
        status,
        detail: detail.into(),
    }
}

#[test]
fn producer_output_matches_the_shared_fixture() {
    let report = ScanReport {
        scan_id: 7,
        reconciled: ReconcileReport {
            new: vec![rf("/usr/bin/sudo", DiffStatus::New, "checksum changed")],
            still_outstanding: vec![rf("kargs", DiffStatus::StillOutstanding, "unexpected karg")],
            resolved: vec![rf("/usr/bin/old", DiffStatus::Resolved, "")],
        },
        informational: vec![Finding {
            check_type: CheckType::LockdownState,
            classification: Classification::Informational,
            path: "secureboot".into(),
            detail: "enabled".into(),
        }],
        skipped: vec![SkippedCheck {
            check: "rootkit",
            reason: "chkrootkit not installed".into(),
        }],
    };
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/report.json")).unwrap();
    assert_eq!(report_json(&report), fixture);
}
