//! The process-wide recorder: nothing is recorded unless a window is running.
//! It is global state, so these tests take one lock.

use super::*;

fn row() -> PendingRow {
    PendingRow {
        process: "curl".into(),
        process_path: Some("/usr/bin/curl".into()),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        port: 443,
        protocol: "tcp".into(),
    }
}

#[test]
fn nothing_is_recorded_without_a_running_window() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Start empty: an ended window keeps its record for review, so another
    // test that ran first (silent_answer_tests ends one) can leave a program
    // behind. A new window clears it; ending that one records nothing.
    set_active(true);
    set_active(false);
    assert_eq!(status(), Status::default());
    record("1:a", &row());
    note_lagged();
    assert_eq!(status().programs_seen, 0);
    assert!(!status().incomplete);
}

#[test]
fn a_running_window_records_and_a_new_window_starts_afresh() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    assert!(set_active(true), "reports that a window began");
    assert!(!set_active(true), "and only once");
    assert!(is_active());
    record("1:a", &row());
    record("1:a", &row());
    note_lagged();
    assert_eq!(status().programs_seen, 1);
    assert_eq!(snapshot().c[&path_key("/usr/bin/curl")].connections, 1);
    assert!(status().incomplete);

    // The window ends: what was seen stays (it is waiting to be reviewed) but
    // nothing more is recorded.
    assert!(!set_active(false));
    record("1:b", &row());
    assert_eq!(snapshot().c[&path_key("/usr/bin/curl")].connections, 1);
    assert_eq!(status().programs_seen, 1);

    // A new window counts afresh.
    assert!(set_active(true));
    assert_eq!(status(), Status::default());
    set_active(false);
}

#[test]
fn the_notice_is_fixed_text_for_the_file_and_the_lock() {
    assert_eq!(notice_text(&Status::default()), "");
    let damaged = Status {
        notice: Some(Notice::Corrupt),
        ..Status::default()
    };
    assert_eq!(notice_text(&damaged), Notice::Corrupt.text());
    let both = Status {
        notice: Some(Notice::WriteFailed),
        lock_degraded: true,
        ..Status::default()
    };
    let text = notice_text(&both);
    assert!(text.starts_with(Notice::WriteFailed.text()), "{text}");
    assert!(text.contains("File locking was not available"), "{text}");
    let lock_only = Status {
        lock_degraded: true,
        ..Status::default()
    };
    assert!(notice_text(&lock_only).starts_with("File locking"));
}
