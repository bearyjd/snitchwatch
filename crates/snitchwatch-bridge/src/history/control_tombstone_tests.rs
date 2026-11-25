//! #441: what a removal that failed past the rename leaves behind. The
//! tombstone holds rows the user was never told were gone, so a clear says
//! so, keeps what it carried, a preview counts the tombstone, and a retry
//! can always start and always says when the rows are still on disk.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::control::{Limits, Refused};
use super::control_clear_tests::{feed_canary, rows, wait_rows};
use super::control_files::Step;
use super::control_leftover_tests::failed_delete_at;
use super::control_support::count;
use super::control_tests::{feed, gate_rig, GateRig};

const TOMBSTONE: &str = "history.db.deleting";

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

/// A rig with one previewed row and one more accepted after the preview.
fn previewed_with_one_to_carry() -> (GateRig, super::control_preview::TokenId) {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    feed(&rig, 1);
    wait_rows(&rig, 2);
    (rig, token)
}

#[test]
fn a_clear_failing_past_the_rename_keeps_the_carried_rows_and_says_data_is_left() {
    let (rig, token) = previewed_with_one_to_carry();
    // The tombstone cannot be unlinked (it is not a plain file any more),
    // while the directory stays writable.
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Renamed {
            let dir = rig.location.dir();
            fs::rename(dir.join(TOMBSTONE), dir.join("moved")).unwrap();
            fs::create_dir(dir.join(TOMBSTONE)).unwrap();
        }
    });
    let Err(Refused::DataLeft(_)) = &result else {
        panic!("{result:?}");
    };
    assert_eq!(rows(&rig), 1, "the row accepted after the preview is kept");
    let host = count(
        &rig.location,
        "SELECT COUNT(*) FROM events WHERE dst_host = 'h0.example'",
    );
    assert_eq!(host, 1);
    assert!(rig.gate.slot().get().is_some(), "recording resumed");
}

#[test]
fn a_clear_failing_past_the_rename_that_cannot_carry_stays_off() {
    let (rig, token) = previewed_with_one_to_carry();
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Renamed {
            chmod(&rig, 0o500);
        }
    });
    chmod(&rig, 0o700);
    let Err(Refused::DataLeft(_)) = &result else {
        panic!("{result:?}");
    };
    assert!(rig.gate.slot().get().is_none(), "recording stays off");
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
    rig.gate.delete(preview.token).unwrap();
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
    assert!(!rig.location.settings_path().exists());
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
