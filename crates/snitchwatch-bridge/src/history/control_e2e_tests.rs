//! PR 10, the parent plan's end-to-end test: a clear, or a disable and
//! re-enable, while prompts are pending. After they settle, nothing from
//! before is written except the gap the gate committed, no pin is left
//! behind, and a prompt accepted afterwards is recorded normally.

use std::sync::Arc;

use rusqlite::Connection as Db;

use super::control::Limits;
use super::control_support::{conn, consent, count};
use super::control_tests::{gate_rig, GateRig};
use super::guard::DecisionOutcome;
use super::record::AnsweredBy;

fn gaps(rig: &GateRig) -> Vec<(String, i64)> {
    let db = Db::open(rig.location.db_path()).unwrap();
    let mut stmt = db
        .prepare("SELECT cause, count FROM gaps ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn settle(guard: super::guard::DecisionGuard) {
    let rule = super::control_support::rule("answered", "allow");
    guard.settle(DecisionOutcome::Replied {
        rule: &rule,
        answered_by: AnsweredBy::Gui,
    });
}

#[test]
fn disable_and_reenable_with_prompts_pending_writes_one_gap_and_nothing_else() {
    let rig = gate_rig(Limits::default());
    let old = rig.gate.slot().get().unwrap();
    let first = old.begin_decision(&conn("/a", "one.example")).unwrap();
    let second = old.begin_decision(&conn("/b", "two.example")).unwrap();
    rig.gate.disable().unwrap();
    assert_eq!(gaps(&rig), vec![("disabled".to_owned(), 2)]);
    // Re-enabled under the very same consent: only the generation tells the
    // old prompts apart.
    rig.gate.enable(consent()).unwrap();
    let new = rig.gate.slot().get().expect("recording again");
    assert!(!Arc::ptr_eq(&old, &new));
    settle(first);
    settle(second);
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM events"), 0);
    // The one gap for the two prompts, then the ordinary `disabled` interval
    // the new session records for the time history was off.
    assert_eq!(
        gaps(&rig),
        vec![("disabled".to_owned(), 2), ("disabled".to_owned(), 1)]
    );
    assert_eq!(new.pins_for_test(), 0, "no pin left behind");
    assert_eq!(old.pins_for_test(), 0, "none leaked by the old capture");
    // A prompt accepted after the operation is recorded normally.
    settle(new.begin_decision(&conn("/c", "three.example")).unwrap());
    let new = rig.gate.slot().get().unwrap();
    super::control_tests::wait_for("the row to commit", || new.pins_for_test() == 0);
    rig.gate.disable().unwrap();
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM events"), 1);
}

#[test]
fn clear_with_prompts_pending_discards_them_and_records_the_next() {
    let rig = gate_rig(Limits::default());
    let old = rig.gate.slot().get().unwrap();
    let pending = old.begin_decision(&conn("/a", "one.example")).unwrap();
    let token = rig.gate.preview().unwrap().token;
    rig.gate.clear(token).unwrap();
    settle(pending);
    assert_eq!(old.pins_for_test(), 0, "the pin left with the old store");
    let new = rig.gate.slot().get().expect("still recording");
    assert_eq!(new.pins_for_test(), 0);
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM events"), 0);
    assert_eq!(
        gaps(&rig),
        vec![],
        "a cleared store says nothing about before"
    );
    settle(new.begin_decision(&conn("/c", "two.example")).unwrap());
    rig.gate.disable().unwrap();
    assert_eq!(count(&rig.location, "SELECT COUNT(*) FROM events"), 1);
    assert_eq!(
        count(
            &rig.location,
            "SELECT COUNT(*) FROM events WHERE dst_host = 'one.example'"
        ),
        0
    );
}
