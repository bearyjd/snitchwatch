//! Test support for the retention suites: a small-budget engine on a tempdir
//! and builders for events, snapshots and raw fills.

use std::sync::{Arc, Mutex};

use super::budget::Budget;
use super::caps::{DstHost, ProcessPath, Snapshot, RULE_HASH};
use super::ledger::{read_gaps, Gap, GapCause};
use super::measure::estimate_bytes;
use super::record::{insert_event, Action, AnsweredBy, EventRecord, SnapshotRecord};
use super::retention::Victims;
use super::test_support::{state_dir, tempdir};
use super::writer::{Engine, Item, TxnReport, WriteError};
use super::{enable, locate, open_store, Consent, Location, Mode};

pub(super) struct Rig {
    pub(super) _root: tempfile::TempDir,
    pub(super) location: Location,
    pub(super) engine: Engine,
    pub(super) reports: Arc<Mutex<Vec<TxnReport>>>,
}

pub(super) fn rig() -> Rig {
    rig_with(16 * 1024 * 1024)
}

pub(super) fn rig_with(total: u64) -> Rig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let consent = Consent {
        text_id: "t".into(),
        accepted_ms: 1,
    };
    enable(&location, consent).unwrap();
    let store = open_store(&location).unwrap();
    let mut engine = Engine::new(store, &location, Budget::new(total).unwrap()).unwrap();
    engine
        .connection()
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    let reports = Arc::new(Mutex::new(Vec::new()));
    let sink = reports.clone();
    engine.set_observer(Box::new(move |r| sink.lock().unwrap().push(*r)));
    Rig {
        _root: root,
        location,
        engine,
        reports,
    }
}

pub(super) fn hash(n: u8) -> [u8; RULE_HASH] {
    [n; RULE_HASH]
}

pub(super) fn event(ts: i64, process: &str, action: Action) -> EventRecord {
    let mut e = EventRecord::new(ts, action, AnsweredBy::Rule);
    e.process_path = Some(ProcessPath::new(process));
    e
}

pub(super) fn with_rule(mut e: EventRecord, n: u8) -> Item {
    e.rule_hash = Some(hash(n));
    Item {
        snapshot: Some(SnapshotRecord::for_test(
            hash(n),
            Snapshot::new(&format!("{{\"rule\":{n}}}")).unwrap(),
            e.ts_ms,
        )),
        event: e,
    }
}

pub(super) fn plain(e: EventRecord) -> Item {
    Item {
        event: e,
        snapshot: None,
    }
}

/// Inserts `items` through the writer, `rows_per_txn` at a time.
pub(super) fn put(rig: &mut Rig, items: Vec<Item>) {
    for chunk in items.chunks(rig.engine.budget().rows_per_txn as usize) {
        rig.engine.insert_batch(chunk).unwrap();
    }
}

pub(super) fn count(rig: &Rig, table: &str) -> i64 {
    rig.engine
        .connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

pub(super) fn remaining(rig: &Rig) -> Vec<(i64, i64)> {
    rig.engine
        .connection()
        .prepare("SELECT ts_ms, id FROM events ORDER BY ts_ms, id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

pub(super) fn snapshots(rig: &Rig) -> Vec<Vec<u8>> {
    rig.engine
        .connection()
        .prepare("SELECT rule_hash FROM rules_seen ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

pub(super) fn evicted(rig: &Rig) -> Vec<Gap> {
    read_gaps(rig.engine.connection())
        .unwrap()
        .into_iter()
        .filter(|g| g.cause == GapCause::Evicted)
        .collect()
}

/// Every removed event's instant lies inside some `evicted` row: the view
/// never shows a silent empty interval where events used to be.
pub(super) fn assert_removed_are_covered(rig: &Rig, removed_ts: &[i64]) {
    let rows = evicted(rig);
    for ts in removed_ts {
        assert!(
            rows.iter().any(|g| g.start_ms <= *ts && *ts <= g.end_ms),
            "event at {ts} was removed without an evicted row covering it: {rows:?}"
        );
    }
}

pub(super) fn fail_once() -> Box<dyn FnMut() -> Option<WriteError> + Send> {
    let mut armed = true;
    Box::new(move || {
        std::mem::take(&mut armed).then(|| WriteError::Io(std::io::Error::other("injected")))
    })
}

pub(super) fn used(rig: &Rig) -> u64 {
    rig.engine.used_pages().unwrap()
}

pub(super) fn high_water(rig: &Rig) -> u64 {
    rig.engine.budget().high_water_pages()
}

pub(super) fn share_bytes(rig: &Rig) -> u64 {
    rig.engine.budget().main_bytes / 100 * super::admission::SHARE_PERCENT
}

pub(super) fn bytes_of(rig: &Rig, process: &str) -> u64 {
    rig.engine
        .process_bytes()
        .unwrap()
        .into_iter()
        .find(|(p, _)| p == process)
        .map_or(0, |(_, b)| b)
}

pub(super) fn count_of(rig: &Rig, process: &str) -> i64 {
    rig.engine
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE coalesce(process_path, '') = ?1",
            [process],
            |r| r.get(0),
        )
        .unwrap()
}

/// Inserts `events` in one raw transaction: no room is made, so a test can
/// build any state up to the file's cap.
pub(super) fn fill(rig: &Rig, events: impl IntoIterator<Item = EventRecord>) {
    let conn = rig.engine.connection();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    for e in events {
        insert_event(conn, &e).unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
}

pub(super) fn hostile_path() -> String {
    format!("/opt/{}", "p".repeat(4000))
}

/// A record of worst-case size from the flooding process.
pub(super) fn big(ts: i64) -> EventRecord {
    let mut e = event(ts, &hostile_path(), Action::Allow);
    e.dst_host = Some(DstHost::new(&"h".repeat(250)));
    e
}

pub(super) fn small_for(ts: i64, process: &str) -> EventRecord {
    let mut e = event(ts, process, Action::Allow);
    e.dst_host = Some(DstHost::new(&"s".repeat(200)));
    e
}

/// Rows from `n` processes in rotation, until the file passes high-water by
/// `over` pages. Timestamps follow insertion order.
pub(super) fn fill_rotating(rig: &Rig, n: usize, over: u64, first_ts: i64) {
    let mut ts = first_ts;
    while used(rig) <= high_water(rig) + over {
        let rows: Vec<EventRecord> = (0..32)
            .map(|i| {
                ts += 1;
                small_for(ts, &format!("/usr/bin/p{}", (ts as usize + i) % n))
            })
            .collect();
        fill(rig, rows);
    }
}

pub(super) fn row_bytes_of(process: &str) -> u64 {
    estimate_bytes(&event(0, process, Action::Allow))
}

/// One chunk of the events older than `cutoff_ms` (tier 1).
pub(super) fn evict_before(rig: &mut Rig, cutoff_ms: i64) -> Result<TxnReport, WriteError> {
    rig.engine
        .evict_victims(Victims::Before(cutoff_ms), u64::MAX)
        .map(|(report, _)| report)
}

/// One chunk of `process`'s events, oldest first (tier 2).
pub(super) fn evict_process(rig: &mut Rig, process: &str) -> Result<TxnReport, WriteError> {
    rig.engine
        .evict_victims(Victims::Process(process), u64::MAX)
        .map(|(report, _)| report)
}

/// Tiny rows from many processes until eviction cannot make room.
pub(super) fn fill_until_paused(rig: &mut Rig) {
    let mut ts = 0;
    while used(rig) <= high_water(rig) + 200 {
        fill(
            rig,
            (0..32).map(|_| {
                ts += 1;
                event(ts, &format!("/usr/bin/p{}", ts % 40), Action::Allow)
            }),
        );
    }
    assert!(!rig.engine.make_room().unwrap());
}
