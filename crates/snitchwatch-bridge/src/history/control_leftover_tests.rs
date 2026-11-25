//! PR 10 repair (M-a, M-b): a delete that stops with the tombstone still on
//! disk says so, a preview never reports it as empty, a retry finishes it,
//! and a delete revokes consent in the same write that turns recording off.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::control::{Limits, Refused};
use super::control_clear_tests::{feed_canary, wait_rows};
use super::control_files::Step;
use super::control_tests::{gate_rig, settings_json, GateRig};

const TOMBSTONE: &str = "history.db.deleting";

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

/// A rig with one row whose delete fails when `step` is reached.
fn failed_delete_at(step: Step) -> (GateRig, Result<(), Refused>) {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    let result = rig.gate.delete_observed(token, &|at| {
        if at == step {
            chmod(&rig, 0o500);
        }
    });
    chmod(&rig, 0o700);
    (rig, result)
}

#[test]
fn a_delete_stopping_on_the_tombstone_says_the_data_is_still_on_disk() {
    let (rig, result) = failed_delete_at(Step::Renamed);
    let Err(Refused::DataLeft(_)) = &result else {
        panic!("{result:?}");
    };
    let text = result.unwrap_err().to_string();
    assert!(text.contains("still on disk"), "{text}");
    assert!(text.contains("retry delete"), "{text}");
    assert!(rig.location.dir().join(TOMBSTONE).exists());
    assert!(rig.gate.slot().get().is_none(), "recording stays off");
}

#[test]
fn a_delete_stopping_on_the_settings_is_incomplete_not_data_left() {
    let (rig, result) = failed_delete_at(Step::TombstoneRemoved);
    let Err(Refused::Incomplete(reason)) = &result else {
        panic!("{result:?}");
    };
    assert!(reason.contains("settings could not be removed"), "{reason}");
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
}

#[test]
fn a_preview_reports_the_leftover_tombstone_instead_of_an_empty_history() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    let preview = rig.gate.preview().unwrap();
    assert!(preview.leftover, "{preview:?}");
    assert_eq!(preview.count, 1, "the rows in the tombstone are counted");
    assert!(preview.approx_bytes > 0);
}

#[test]
fn a_clean_preview_reports_no_leftover() {
    let rig = gate_rig(Limits::default());
    assert!(!rig.gate.preview().unwrap().leftover);
}

#[test]
fn a_retried_delete_removes_the_tombstone_and_settings_with_recording_off() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    let token = rig.gate.preview().unwrap().token;
    assert!(rig.gate.slot().get().is_none());
    rig.gate.delete(token).unwrap();
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
    assert!(!rig.location.settings_path().exists());
    assert!(rig.gate.slot().get().is_none(), "recording stays off");
    assert!(!rig.gate.preview().unwrap().leftover);
}

#[test]
fn a_retried_clear_also_removes_the_tombstone() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    let token = rig.gate.preview().unwrap().token;
    rig.gate.clear(token).unwrap();
    assert!(!rig.location.dir().join(TOMBSTONE).exists());
    assert!(rig.gate.slot().get().is_none(), "recording was off");
}

#[test]
fn a_destructive_operation_finishes_the_leftover_before_its_own_checks() {
    let (rig, _) = failed_delete_at(Step::Renamed);
    // Sidecars of a store that is gone belong to nothing: only the leftover
    // finisher removes them; the removal's own check would refuse.
    fs::write(rig.location.dir().join("history.db-wal"), b"x").unwrap();
    let token = rig.gate.preview().unwrap().token;
    rig.gate.delete(token).unwrap();
    assert!(!rig.location.dir().join("history.db-wal").exists());
}

#[test]
fn a_delete_whose_settings_removal_fails_leaves_no_consent() {
    let (rig, _) = failed_delete_at(Step::TombstoneRemoved);
    let settings = settings_json(&rig);
    assert!(settings.contains("\"enabled\":false"), "{settings}");
    assert!(!settings.contains("text_id"), "consent revoked: {settings}");
    assert!(!settings.contains("accepted_ms"), "{settings}");
}

#[test]
fn a_delete_refused_before_the_rename_restores_the_consent_exactly() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let before = fs::read(rig.location.settings_path()).unwrap();
    let token = rig.gate.preview().unwrap().token;
    // What was previewed changes: the recheck refuses, after the settings
    // were turned off and the writer stopped.
    let db = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    db.execute("UPDATE events SET repeat_count = repeat_count + 1", [])
        .unwrap();
    drop(db);
    assert_eq!(rig.gate.delete(token), Err(Refused::Changed));
    assert_eq!(fs::read(rig.location.settings_path()).unwrap(), before);
    assert!(rig.gate.slot().get().is_some(), "recording is back");
}
