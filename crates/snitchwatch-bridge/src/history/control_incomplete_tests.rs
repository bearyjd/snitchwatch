//! PR 10 review (M1): a delete that has passed the point of no return and
//! then fails never switches recording back on; a clear in the same spot
//! still puts recording back, as the user had it.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::control::{ControlGate, Limits, Refused};
use super::control_clear_tests::{feed_canary, wait_rows};
use super::control_files::Step;
use super::control_tests::{gate_rig, settings_json, GateRig};
use super::Mode;

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

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
fn a_delete_that_fails_after_the_store_is_gone_keeps_recording_off() {
    // Between the rename and the unlink, and after the unlink (settings).
    for step in [Step::Renamed, Step::TombstoneRemoved] {
        let (rig, result) = failed_delete_at(step);
        let past = match step {
            Step::Renamed => matches!(result, Err(Refused::DataLeft(_))),
            _ => matches!(result, Err(Refused::Incomplete(_))),
        };
        assert!(past, "{step:?}: {result:?}");
        assert!(
            rig.gate.slot().get().is_none(),
            "{step:?}: no capture respawned"
        );
        assert!(
            settings_json(&rig).contains("\"enabled\":false"),
            "{step:?}: settings say disabled"
        );
        assert!(
            !rig.location.db_path().exists(),
            "{step:?}: no store recreated"
        );
        assert!(rig.gate.readers().try_acquire().is_ok(), "readers reopened");
        // The next start finishes the removal and records nothing.
        let (state, dir) = (rig.state.clone(), rig.location.dir().to_owned());
        drop(rig.gate);
        let next = ControlGate::start(Mode::User, Some(&state), Limits::default());
        assert!(
            next.slot().get().is_none(),
            "{step:?}: still off after restart"
        );
        assert!(
            !dir.join("history.db.deleting").exists(),
            "{step:?}: tombstone finished"
        );
        assert!(!dir.join("history.db").exists());
    }
}

#[test]
fn a_clear_that_fails_after_the_store_is_gone_still_records_as_before() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    // The tombstone cannot be unlinked (it is a directory now), but the
    // directory stays writable: recording can be put back on an empty store.
    let tombstone = rig.location.dir().join("history.db.deleting");
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Renamed {
            fs::remove_file(&tombstone).unwrap();
            fs::create_dir(&tombstone).unwrap();
        }
    });
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    assert!(rig.gate.slot().get().is_some(), "recording is back");
    let settings = settings_json(&rig);
    assert!(settings.contains("\"enabled\":true"), "{settings}");
    assert!(
        settings.contains("\"text_id\":\"t\""),
        "consent kept: {settings}"
    );
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
}
