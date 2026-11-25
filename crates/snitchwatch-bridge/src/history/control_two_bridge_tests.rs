//! PR 10 (#418) at the gate: only the bridge that holds the writer lock may
//! finish, start or change anything; a second bridge on the same state
//! directory must not even finish the first one's half-done removal.

use std::fs;

use super::control::{ControlGate, Limits, Refused};
use super::control_support::count;
use super::control_tests::gate_rig;
use super::Mode;

#[test]
fn a_second_bridge_records_nothing_and_never_finishes_the_first_ones_removal() {
    let rig = gate_rig(Limits::default());
    let tombstone = rig.location.dir().join("history.db.deleting");
    fs::write(&tombstone, "the first bridge is mid-clear").unwrap();
    let sessions = "SELECT COUNT(*) FROM sessions";
    let before = count(&rig.location, sessions);
    let second = ControlGate::start(Mode::User, Some(&rig.state), Limits::default());
    assert!(
        second.slot().get().is_none(),
        "the second bridge records nothing"
    );
    assert!(matches!(second.preview(), Err(Refused::Unavailable(_))));
    assert!(matches!(second.disable(), Err(Refused::Unavailable(_))));
    assert!(
        tombstone.exists(),
        "the first bridge's tombstone is not ours to remove"
    );
    assert_eq!(
        count(&rig.location, sessions),
        before,
        "its coverage is untouched"
    );
    assert!(rig.gate.slot().get().is_some());
    // Once the first bridge is gone, the next one finishes the job.
    drop(second);
    drop(rig.gate);
    let third = ControlGate::start(Mode::User, Some(&rig.state), Limits::default());
    assert!(!tombstone.exists());
    assert!(third.slot().get().is_some());
}
