//! #441: what a removal that failed past the rename leaves behind. The
//! tombstone holds rows the user was never told were gone, so a clear says
//! so, a preview counts the tombstone, and a retry can always start and
//! always says when the rows are still on disk.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::control::{Limits, Refused};
use super::control_clear_tests::{feed_canary, rows, wait_rows};
use super::control_files::Step;
use super::control_leftover_tests::failed_delete_at;
use super::control_tests::{gate_rig, GateRig};

const TOMBSTONE: &str = "history.db.deleting";

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

/// Only a clear that is not recording and has nothing to keep still goes
/// through the tombstone.
#[test]
fn an_unrecorded_clear_failing_past_the_rename_says_data_is_left_and_claims_no_recording() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    rig.gate.disable().unwrap();
    let token = rig.gate.preview().unwrap().token;
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Renamed {
            chmod(&rig, 0o500);
        }
    });
    chmod(&rig, 0o700);
    let Err(Refused::DataLeft(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(!reason.contains("recording"), "{reason}");
    assert!(rig.gate.slot().get().is_none(), "recording was off");
    assert!(rig.location.dir().join(TOMBSTONE).exists());
}

/// The tombstone of a store, beside the live store: a copy of it.
fn tombstone_beside_store(rig: &GateRig) {
    let dir = rig.location.dir();
    fs::copy(rig.location.db_path(), dir.join(TOMBSTONE)).unwrap();
}

#[test]
fn a_preview_counts_the_tombstone_beside_a_store() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    tombstone_beside_store(&rig);
    let preview = rig.gate.preview().unwrap();
    assert!(preview.leftover);
    assert_eq!(preview.count, 4, "the store's rows and the tombstone's");
    rig.gate.clear(preview.token).unwrap();
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
    assert_eq!(rows(&rig), 0);
}

#[test]
fn a_tombstone_that_changed_after_the_preview_refuses_the_clear() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    tombstone_beside_store(&rig);
    let token = rig.gate.preview().unwrap().token;
    let tomb = rusqlite::Connection::open(rig.location.dir().join(TOMBSTONE)).unwrap();
    tomb.execute("UPDATE events SET repeat_count = repeat_count + 1", [])
        .unwrap();
    drop(tomb);
    assert_eq!(rig.gate.clear(token), Err(Refused::Changed));
    assert!(rig.location.dir().join(TOMBSTONE).exists(), "not unlinked");
    assert_eq!(rows(&rig), 2);
}

#[test]
fn a_tombstone_that_appeared_after_the_preview_refuses_the_clear() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let token = rig.gate.preview().unwrap().token;
    tombstone_beside_store(&rig);
    assert_eq!(rig.gate.clear(token), Err(Refused::Changed));
    assert!(rig.location.dir().join(TOMBSTONE).exists());
}

#[test]
fn a_tombstone_that_cannot_be_scanned_still_gets_a_token_to_retry_with() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    // Another name for the tombstone: the scan refuses a file with a link.
    fs::hard_link(
        rig.location.dir().join(TOMBSTONE),
        rig.location.dir().join("other-name"),
    )
    .unwrap();
    let preview = rig.gate.preview().expect("a retry must be able to start");
    assert!(preview.leftover);
    assert_eq!(preview.count, 0, "nothing could be counted");
    // The other name keeps a full copy: the delete does not claim success.
    let result = rig.gate.delete(preview.token);
    let Err(Refused::DataLeft(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("another link"), "{reason}");
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
}

/// A preview of a store beside a tombstone that `make` left unscannable:
/// refused, with the reason.
fn refusal_beside_a_store(make: impl Fn(&std::path::Path)) -> String {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    make(rig.location.dir());
    match rig.gate.preview() {
        Err(Refused::Unavailable(reason)) => reason,
        other => panic!("its rows would go uncounted and unchecked: {other:?}"),
    }
}

#[test]
fn a_tombstone_that_cannot_be_scanned_beside_a_store_gets_no_token() {
    // Damaged: the next start unlinks it.
    let reason = refusal_beside_a_store(|dir| {
        fs::write(dir.join(TOMBSTONE), [7u8; 4096]).unwrap();
    });
    assert!(
        reason.contains("restart the bridge to finish removing it"),
        "{reason}"
    );
    // A second link: a restart removes only this name.
    let reason = refusal_beside_a_store(|dir| {
        fs::write(dir.join(TOMBSTONE), b"old").unwrap();
        fs::hard_link(dir.join(TOMBSTONE), dir.join("other-name")).unwrap();
    });
    assert!(reason.contains("another link may keep"), "{reason}");
    // Not a plain file: a restart leaves it where it is.
    let not_plain: [fn(&std::path::Path); 2] = [
        |dir| fs::create_dir(dir.join(TOMBSTONE)).unwrap(),
        |dir| std::os::unix::fs::symlink("history.db", dir.join(TOMBSTONE)).unwrap(),
    ];
    for make in not_plain {
        let reason = refusal_beside_a_store(make);
        assert!(reason.contains("not a plain file"), "{reason}");
        assert!(reason.contains("by hand, then restart"), "{reason}");
    }
}

#[test]
fn a_retry_blocked_by_a_non_plain_tombstone_does_not_claim_recording_is_off() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    // No store, and a tombstone that is not a file: nothing can be scanned.
    let dir = rig.location.dir();
    fs::remove_file(rig.location.db_path()).unwrap();
    fs::create_dir(dir.join(TOMBSTONE)).unwrap();
    let token = rig.gate.preview().unwrap().token;
    let result = rig.gate.delete(token);
    let Err(Refused::DataLeft(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(
        !reason.contains("stays off"),
        "recording is still on: {reason}"
    );
    assert!(rig.gate.slot().get().is_some());
}

#[test]
fn a_retry_that_cannot_unlink_the_tombstone_still_says_the_data_is_on_disk() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    let token = rig.gate.preview().unwrap().token;
    chmod(&rig, 0o500);
    let result = rig.gate.delete(token);
    chmod(&rig, 0o700);
    let Err(Refused::DataLeft(_)) = &result else {
        panic!("{result:?}");
    };
    assert!(rig.location.dir().join(TOMBSTONE).exists());
}

#[test]
fn recovery_still_rolls_back_a_hot_journal_when_the_tombstone_has_a_second_link() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    rig.gate.disable().unwrap();
    let GateRig { location, gate, .. } = rig;
    drop(gate);
    let dir = location.dir();
    fs::write(dir.join(TOMBSTONE), b"old").unwrap();
    fs::hard_link(dir.join(TOMBSTONE), dir.join("other-name")).unwrap();
    fs::write(dir.join("history.db-journal"), [7u8; 1024]).unwrap();
    super::control_files::recover(&location);
    assert!(!dir.join(TOMBSTONE).exists(), "the leftover was finished");
    assert!(
        !dir.join("history.db-journal").exists(),
        "the journal was rolled back, not skipped"
    );
}
