//! PR 10 review: what a refused control operation leaves behind. A refusal
//! changes nothing the user can see, never strands a prompt that was being
//! answered, never creates or keeps a settings file, and says so when it
//! could not put recording back.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::control::{ControlGate, Limits, Refused};
use super::control_clear_tests::{feed_canary, wait_rows};
use super::control_files::{Step, StoreDir};
use super::control_support::{conn, consent, count};
use super::control_tests::{gate_rig, wait_for, GateRig};
use super::guard::DecisionOutcome;
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, Consent, Location, Mode};

fn chmod(rig: &GateRig, mode: u32) {
    fs::set_permissions(rig.location.dir(), fs::Permissions::from_mode(mode)).unwrap();
}

fn rows(rig: &GateRig) -> i64 {
    count(&rig.location, "SELECT COUNT(*) FROM events")
}

fn answered(rig: &GateRig, op: impl FnOnce(&GateRig) -> Result<(), Refused>) {
    let capture = rig.gate.slot().get().expect("recording");
    let before = rig.gate.generation().current();
    let guard = capture
        .begin_decision(&conn("/usr/bin/curl", "pending.example"))
        .unwrap();
    chmod(rig, 0o500);
    let result = op(rig);
    chmod(rig, 0o700);
    assert!(matches!(result, Err(Refused::Settings(_))), "{result:?}");
    assert_eq!(
        rig.gate.generation().current(),
        before,
        "a refusal changes nothing, the generation included"
    );
    guard.settle(DecisionOutcome::GuiLost);
    wait_for("the answer to be written", || rows(rig) == 1);
    wait_for("its pin to be released", || capture.pins_for_test() == 0);
}

#[test]
fn a_refused_disable_does_not_strand_a_prompt_that_was_pending() {
    answered(&gate_rig(Limits::default()), |rig| rig.gate.disable());
}

#[test]
fn a_refused_delete_does_not_strand_a_prompt_that_was_pending() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    let capture = rig.gate.slot().get().unwrap();
    let guard = capture
        .begin_decision(&conn("/usr/bin/curl", "pending.example"))
        .unwrap();
    chmod(&rig, 0o500);
    let result = rig.gate.delete(token);
    chmod(&rig, 0o700);
    // The directory is refused before anything is closed or written.
    assert!(matches!(result, Err(Refused::Files(_))), "{result:?}");
    guard.settle(DecisionOutcome::GuiLost);
    wait_rows(&rig, 2);
    wait_for("its pin to be released", || capture.pins_for_test() == 0);
}

/// Stops the bridge, lets `edit` change the files it left, starts a new one.
fn restart(rig: GateRig, edit: impl FnOnce(&Location)) -> GateRig {
    let GateRig {
        root,
        state,
        location,
        gate,
    } = rig;
    drop(gate);
    edit(&location);
    let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
    GateRig {
        root,
        state,
        location,
        gate,
    }
}

#[test]
fn a_store_with_unreadable_settings_can_still_be_deleted() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let rig = restart(rig, |location| {
        fs::write(location.settings_path(), "{\"enabled\":tr").unwrap();
    });
    assert!(rig.gate.slot().get().is_none(), "unreadable settings: off");
    let token = rig.gate.preview().unwrap().token;
    rig.gate.delete(token).unwrap();
    let dir = rig.location.dir();
    assert!(!dir.join("history.db").exists());
    assert!(!dir.join("settings.json").exists(), "removed by its name");
}

#[test]
fn a_refused_delete_never_creates_a_settings_file() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let rig = restart(rig, |location| {
        fs::remove_file(location.settings_path()).unwrap();
    });
    let token = rig.gate.preview().unwrap().token;
    let db = rusqlite::Connection::open(rig.location.db_path()).unwrap();
    db.execute("UPDATE events SET repeat_count = repeat_count + 1", [])
        .unwrap();
    drop(db);
    assert_eq!(rig.gate.delete(token), Err(Refused::Changed));
    assert!(
        !rig.location.settings_path().exists(),
        "a refusal must not leave a settings file that was never there"
    );
}

#[test]
fn a_second_bridge_that_started_before_history_existed_cannot_rewrite_consent() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    // Started with no history directory, so it holds no lock yet.
    let second = ControlGate::start(Mode::User, Some(&state), Limits::default());
    // The first bridge enables history and takes the lock.
    enable(&location, consent()).unwrap();
    let held = StoreDir::open(location.dir(), super::open::effective_uid())
        .unwrap()
        .lock_writer()
        .unwrap();
    let before = fs::read(location.settings_path()).unwrap();
    let other = Consent {
        text_id: "someone-else".into(),
        accepted_ms: 99,
    };
    assert!(matches!(second.enable(other), Err(Refused::Unavailable(_))));
    assert_eq!(
        fs::read(location.settings_path()).unwrap(),
        before,
        "a consent write without owning the store"
    );
    drop(held);
}

#[test]
fn a_delete_failing_past_the_rename_is_incomplete_and_not_rolled_back() {
    for step in [Step::Renamed, Step::TombstoneRemoved] {
        let rig = gate_rig(Limits::default());
        feed_canary(&rig, 1);
        wait_rows(&rig, 1);
        let token = rig.gate.preview().unwrap().token;
        // From here the directory refuses every write: the removal stops,
        // and so does restoring the settings.
        let result = rig.gate.delete_observed(token, &|at| {
            if at == step {
                chmod(&rig, 0o500);
            }
        });
        chmod(&rig, 0o700);
        let past = match step {
            Step::Renamed => matches!(result, Err(Refused::DataLeft(_))),
            _ => matches!(result, Err(Refused::Incomplete(_))),
        };
        assert!(
            past,
            "{step:?}: a delete past the rename is not rolled back: {result:?}"
        );
    }
}

#[test]
fn a_failure_after_the_new_store_exists_does_not_claim_nothing_changed() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    feed_canary(&rig, 1);
    wait_rows(&rig, 2);
    let path = rig.location.db_path();
    // The new store is destroyed before the carry goes into it.
    let result = rig.gate.clear_observed(token, &|at| {
        if at == Step::Created {
            fs::write(&path, vec![b'x'; 4096]).unwrap();
        }
    });
    assert!(
        matches!(result, Err(Refused::RollbackFailed { .. })),
        "{result:?}"
    );
}
