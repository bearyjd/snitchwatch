//! PR 10 review: the authoritative recheck at confirm, what a clear carries
//! into the new store, and a clear or delete while history is off.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::Connection as Db;

use super::control::{Limits, Refused};
use super::control_clear::EVENT_COLUMNS;
use super::control_clear_tests::{feed_canary, rows, wait_rows};
use super::control_preview::{scan_events, Leftover, Token};
use super::control_support::start_after_drop;
use super::control_tests::{feed, gate_rig, settings_json, GateRig};

fn db(rig: &GateRig) -> Db {
    let db = Db::open(rig.location.db_path()).unwrap();
    db.busy_timeout(Duration::from_secs(10)).unwrap();
    db
}

/// Every column of `events` but its id, from the schema itself, so a column
/// the code under test forgets is still compared here.
fn columns(rig: &GateRig) -> String {
    let db = db(rig);
    let mut stmt = db.prepare("PRAGMA table_info(events)").unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .filter(|n| n != "id")
        .collect();
    names.join(", ")
}

/// Adds `n` copies of the first event, straight into the store.
fn grow(rig: &GateRig, n: i64) {
    db(rig)
        .execute(
            &format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {n}) \
                 INSERT INTO events ({EVENT_COLUMNS}) \
                 SELECT {EVENT_COLUMNS} FROM events, n WHERE events.id = 1"
            ),
            [],
        )
        .unwrap();
}

fn carried_rows(rig: &GateRig, after: i64) -> Vec<String> {
    let cols = columns(rig);
    let db = db(rig);
    let width = cols.split(',').count();
    let mut stmt = db
        .prepare(&format!(
            "SELECT {cols} FROM events WHERE id > ?1 ORDER BY id"
        ))
        .unwrap();
    stmt.query_map([after], |r| {
        let all: Vec<Value> = (0..width).map(|i| r.get(i)).collect::<Result<_, _>>()?;
        Ok(format!("{all:?}"))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn rules(rig: &GateRig) -> Vec<String> {
    let db = db(rig);
    let mut stmt = db
        .prepare("SELECT rule_hash, snapshot, first_seen_ms FROM rules_seen ORDER BY rule_hash")
        .unwrap();
    stmt.query_map([], |r| {
        Ok(format!(
            "{:?} {:?} {:?}",
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn the_recheck_is_not_cut_by_the_previews_statement_budget() {
    let limits = Limits {
        statement: Duration::ZERO,
        ..Limits::default()
    };
    let rig = gate_rig(limits);
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    grow(&rig, 6000);
    // A token as an unbounded scan would have issued it: with a zero budget
    // the advisory preview itself could not run.
    let mut conn = crate::sqlite_file::open_owner_only_readonly(&rig.location.db_path()).unwrap();
    let scan = scan_events(&mut conn, None, Arc::new(|| false), None).unwrap();
    let token = rig.gate.0.tokens.issue(Token {
        generation: rig.gate.generation().current(),
        max_id: scan.max_id,
        checksum: scan.checksum,
        leftover: Leftover::Absent,
    });
    drop(conn);
    rig.gate
        .clear(token)
        .expect("the writer is stopped: the recheck is not bounded");
    assert_eq!(rows(&rig), 0);
}

#[test]
fn what_a_clear_carries_is_the_row_and_its_rule_exactly() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    let token = rig.gate.preview().unwrap().token;
    feed(&rig, 2);
    wait_rows(&rig, 4);
    let sent = carried_rows(&rig, 2);
    let rules_before = rules(&rig);
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter().all(|r| r.contains("Blob")),
        "the rows have a rule hash to lose: {sent:?}"
    );
    rig.gate.clear(token).unwrap();
    assert_eq!(carried_rows(&rig, 0), sent, "column for column");
    assert_eq!(rules(&rig), rules_before, "the rule snapshot comes along");
}

#[test]
fn more_than_the_carry_limit_refuses_with_its_own_reason_and_keeps_the_store() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    let token = rig.gate.preview().unwrap().token;
    grow(&rig, 10_001);
    assert_eq!(rig.gate.clear(token), Err(Refused::TooManyCarried));
    assert_eq!(rows(&rig), 10_002, "nothing was removed");
    assert!(rig.gate.slot().get().is_some(), "recording went on");
}

#[test]
fn clear_and_delete_work_while_history_is_off_and_when_the_store_is_missing() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 2);
    wait_rows(&rig, 2);
    rig.gate.disable().unwrap();
    let token = rig.gate.preview().unwrap().token;
    rig.gate.clear(token).unwrap();
    assert!(!rig.location.db_path().exists());
    assert!(rig.gate.slot().get().is_none(), "clear does not turn it on");
    let kept = settings_json(&rig);
    assert!(kept.contains("\"enabled\":false") && kept.contains("\"text_id\":\"t\""));
    // No store at all: the preview counts nothing and delete still finishes.
    let again = rig.gate.preview().unwrap();
    assert_eq!(again.count, 0);
    rig.gate.delete(again.token).unwrap();
    assert!(!rig.location.settings_path().exists());
}

#[test]
fn a_hot_journal_left_while_history_is_off_does_not_block_removal_for_good() {
    let rig = gate_rig(Limits::default());
    feed_canary(&rig, 1);
    wait_rows(&rig, 1);
    rig.gate.disable().unwrap();
    let GateRig {
        root,
        state,
        location,
        gate,
    } = rig;
    drop(gate);
    // What a kill during a commit left behind, found by the next start.
    fs::write(location.dir().join("history.db-journal"), [7u8; 1024]).unwrap();
    let gate = start_after_drop(&state, Limits::default());
    let rig = GateRig {
        root,
        state,
        location,
        gate,
    };
    let token = rig
        .gate
        .preview()
        .expect("the user can see what there is before removing it")
        .token;
    rig.gate.clear(token).unwrap();
    assert!(!rig.location.db_path().exists());
    assert!(!rig.location.dir().join("history.db-journal").exists());
}
