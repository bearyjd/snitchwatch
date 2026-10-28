//! The writer under worst-case rows: page budget per transaction (S2), file
//! sizes against B (S3), shrink on free (S4), canary scan (S5), a
//! non-blocking queue, and the fill/depth invariant the S2 proof rests on.
//! Budgets are small (16 MiB) and databases live under the target directory.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::budget::*;
use super::caps::*;
use super::record::*;
use super::test_support::{state_dir, tempdir};
use super::writer::*;
use super::*;

const MIB: u64 = 1024 * 1024;

struct Rig {
    _root: tempfile::TempDir,
    location: Location,
    engine: Engine,
    reports: Arc<Mutex<Vec<TxnReport>>>,
}

fn rig(total: u64) -> Rig {
    rig_with(total, false)
}

/// `fast`: `synchronous=OFF`, for the tests whose subject is not durability.
/// The S2 tests use the production setting (FULL), which changes how the
/// journal is synced.
fn rig_with(total: u64, fast: bool) -> Rig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(
        &location,
        Consent {
            text_id: "t".into(),
            accepted_ms: 1,
        },
    )
    .unwrap();
    let store = open_store(&location).unwrap();
    let mut engine = Engine::new(store, &location, Budget::new(total).unwrap()).unwrap();
    if fast {
        engine
            .connection()
            .pragma_update(None, "synchronous", "OFF")
            .unwrap();
    }
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

fn scramble(i: u64) -> u64 {
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 29;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^ (x >> 32)
}

fn hash_of(i: u64) -> [u8; RULE_HASH] {
    let mut h = [0u8; RULE_HASH];
    h[..8].copy_from_slice(&scramble(i).to_be_bytes());
    h[8..].copy_from_slice(&scramble(i ^ 0xFFFF).to_be_bytes());
    h
}

/// Exactly `len` bytes, unique per `(i, salt)`.
fn text(prefix: &str, i: u64, len: usize) -> String {
    let mut s = format!("{prefix}{i:020}");
    let mut k = i;
    while s.len() < len {
        k = scramble(k);
        s.push((b'a' + (k % 26) as u8) as char);
    }
    s.truncate(len);
    s
}

/// Every field at its cap, a snapshot at its cap with a hash of its own.
fn worst_item(i: u64) -> Item {
    let ip = IpAddr::V6(Ipv6Addr::new(
        0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
    ));
    let mut e = EventRecord::new(
        i as i64,
        Action::PendingUnanswered,
        AnsweredBy::FilterPaused,
    );
    e.decided_ts_ms = Some(i64::MAX);
    e.process_path = Some(ProcessPath::new(&text("/p", i, PROCESS_PATH)));
    e.dst_host = Some(DstHost::new(&text("h", i, DST_HOST)));
    e.dst_ip = Some(ip);
    e.dst_port = Some(65535);
    e.protocol = Some(Protocol::new(&text("p", i, PROTOCOL)));
    e.rule_name = Some(RuleName::new(&text("r", i, RULE_NAME)));
    e.rule_hash = Some(hash_of(i));
    e.rule_class = Some(RuleClass::new(&text("c", i, RULE_CLASS)));
    e.repeat_count = u32::MAX;
    e.src_port = Some(65535);
    e.src_ip = Some(ip);
    Item {
        snapshot: Some(SnapshotRecord {
            rule_hash: hash_of(i),
            snapshot: Snapshot::new(&text("{", i, SNAPSHOT)).unwrap(),
            first_seen_ms: i as i64,
        }),
        event: e,
    }
}

fn small_item(i: u64) -> Item {
    let mut e = EventRecord::new(i as i64, Action::Allow, AnsweredBy::Rule);
    e.rule_hash = Some(hash_of(i % 500));
    e.dst_port = Some((i % 65535) as u16);
    Item {
        event: e,
        snapshot: None,
    }
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// The bound a transaction's journal must stay under, from its kind and size.
fn bound_for(budget: &Budget, r: &TxnReport) -> u64 {
    let pages = match r.kind {
        TxnKind::Insert | TxnKind::Evict => budget.pages_for_rows(r.units),
        TxnKind::Vacuum => budget.fixed_pages + r.units * VACUUM_STEP_PAGES,
    };
    JOURNAL_HEADER + pages * JOURNAL_RECORD
}

fn assert_all_within_budget(rig: &Rig) {
    let budget = *rig.engine.budget();
    for r in rig.reports.lock().unwrap().iter() {
        assert!(
            r.journal_bytes <= bound_for(&budget, r),
            "{r:?} exceeded its derived bound {}",
            bound_for(&budget, r)
        );
        assert!(r.journal_bytes <= budget.journal_limit_bytes(), "{r:?}");
    }
}

/// Fill past the budget so eviction and vacuum run several times.
fn churn(rig: &mut Rig, rows: u64, item: impl Fn(u64) -> Item) {
    let n = rig.engine.budget().rows_per_txn;
    let mut i = 0;
    while i < rows {
        assert!(
            rig.engine.make_room().unwrap(),
            "recording paused at row {i}"
        );
        let batch: Vec<Item> = (i..(i + n).min(rows)).map(&item).collect();
        rig.engine.insert_batch(&batch).unwrap();
        i += n;
    }
}

// ---- S2 ---------------------------------------------------------------

#[test]
fn a_journal_is_a_sector_header_plus_whole_page_records() {
    let mut rig = rig(16 * MIB);
    rig.engine.insert_batch(&[worst_item(1)]).unwrap();
    let r = rig.reports.lock().unwrap()[0];
    // The header is one sector: 512 bytes under SQLITE_IOCAP_POWERSAFE_OVERWRITE
    // (the unix default, `setSectorSize`), at most 4096 otherwise; the budget
    // assumes the larger.
    let header = [512, 4096]
        .into_iter()
        .find(|h| (r.journal_bytes - h).is_multiple_of(JOURNAL_RECORD))
        .unwrap_or_else(|| panic!("{r:?} is not header + whole 4104-byte records"));
    assert!(header <= JOURNAL_HEADER);
    assert!(r.journal_bytes > header);
}

#[test]
fn worst_case_rows_never_modify_more_pages_than_the_budget() {
    let mut rig = rig(16 * MIB);
    let budget = *rig.engine.budget();
    // Several times the main file's capacity: inserts, evictions, vacuums.
    let rows = budget.max_page_count * PAGE / 14_000 * 3 / 2;
    churn(&mut rig, rows, worst_item);
    assert_all_within_budget(&rig);
    let reports = rig.reports.lock().unwrap();
    for kind in [TxnKind::Insert, TxnKind::Evict, TxnKind::Vacuum] {
        assert!(reports.iter().any(|r| r.kind == kind), "{kind:?} never ran");
    }
    let worst = reports.iter().map(|r| r.journal_bytes).max().unwrap();
    println!(
        "worst journal {worst} B of limit {}",
        budget.journal_limit_bytes()
    );
}

#[test]
fn a_batch_over_the_row_budget_is_refused() {
    let mut rig = rig(16 * MIB);
    let n = rig.engine.budget().rows_per_txn;
    let batch: Vec<Item> = (0..=n).map(worst_item).collect();
    assert!(matches!(
        rig.engine.insert_batch(&batch),
        Err(WriteError::BatchTooLarge { .. })
    ));
}

#[test]
fn a_transaction_over_the_journal_budget_is_rolled_back_not_committed() {
    // The tripwire: a budget whose journal is smaller than what one worst-case
    // row really writes (a second connection; the rig's engine is idle).
    let rig = rig(16 * MIB);
    let mut tiny = *rig.engine.budget();
    tiny.journal_pages = 1;
    let store = open_store(&rig.location).unwrap();
    let mut engine = Engine::new(store, &rig.location, tiny).unwrap();
    let result = engine.insert_batch(&[worst_item(1)]);
    assert!(
        matches!(result, Err(WriteError::BudgetExceeded { .. })),
        "{result:?}"
    );
    let rows: i64 = engine
        .connection()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "rolled back");
}

// ---- S3 / S4 / S5 -----------------------------------------------------

#[test]
fn main_file_plus_peak_journal_plus_settings_plus_slack_stays_within_b() {
    let total = 16 * MIB;
    let mut rig = rig_with(total, true);
    let budget = *rig.engine.budget();
    // The settings file at its maximum, with its atomic replacement in flight.
    let settings = rig.location.settings_path();
    let mut replacement = settings.clone().into_os_string();
    replacement.push(".tmp");
    let blob = vec![b' '; super::settings::SETTINGS_MAX_BYTES as usize];
    std::fs::write(&settings, &blob).unwrap();
    std::fs::write(&replacement, &blob).unwrap();
    let allocated = |p: &std::path::Path| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(p).unwrap().blocks() * 512
    };
    let settings_peak = allocated(&settings) + allocated(std::path::Path::new(&replacement));
    assert!(settings_peak <= budget.settings_bytes);

    let rows = budget.max_page_count * PAGE / 14_000 * 3;
    let mut main_peak = 0;
    let n = budget.rows_per_txn;
    let mut i = 0;
    while i < rows {
        assert!(rig.engine.make_room().unwrap());
        let batch: Vec<Item> = (i..(i + n).min(rows)).map(worst_item).collect();
        rig.engine.insert_batch(&batch).unwrap();
        main_peak = main_peak.max(file_len(&rig.location.db_path()));
        i += n;
    }
    let journal_peak = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.journal_bytes)
        .max()
        .unwrap();
    assert!(main_peak <= budget.max_page_count * PAGE);
    let total_peak = main_peak + journal_peak + settings_peak + budget.slack_bytes;
    println!("main {main_peak} + journal {journal_peak} + settings {settings_peak} + slack {} = {total_peak} of {total}", budget.slack_bytes);
    assert!(total_peak <= total);
}

#[test]
fn the_main_file_cannot_grow_past_its_share_even_without_eviction() {
    // `max_page_count` is the enforcement for S3, not the eviction policy.
    let mut rig = rig_with(16 * MIB, true);
    let budget = *rig.engine.budget();
    let n = budget.rows_per_txn;
    let mut i = 0;
    let failure = loop {
        let batch: Vec<Item> = (i..i + n).map(worst_item).collect();
        match rig.engine.insert_batch(&batch) {
            Ok(_) => i += n,
            Err(e) => break e,
        }
        assert!(i < 100_000, "the file never filled");
    };
    assert!(format!("{failure}").contains("full"), "{failure}");
    assert!(file_len(&rig.location.db_path()) <= budget.max_page_count * PAGE);
}

#[test]
fn freed_space_returns_to_the_filesystem_in_budgeted_steps() {
    let mut rig = rig_with(16 * MIB, true);
    let budget = *rig.engine.budget();
    churn(&mut rig, 400, worst_item);
    let full = file_len(&rig.location.db_path());
    // Delete 90%.
    let keep = 40;
    while rig
        .engine
        .connection()
        .query_row::<i64, _, _>("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
        > keep
    {
        rig.engine.evict_chunk().unwrap();
    }
    rig.engine.vacuum_all().unwrap();
    let shrunk = file_len(&rig.location.db_path());
    assert!(shrunk < full / 5, "{shrunk} of {full}");
    assert_all_within_budget(&rig);
    let steps = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.kind == TxnKind::Vacuum)
        .count();
    assert!(steps > 1, "vacuum ran in several budgeted transactions");
    let _ = budget;
}

#[test]
fn deleted_content_is_not_left_in_the_file() {
    let mut rig = rig(16 * MIB);
    let canary = "/canary-5f3c9a1e-delete-me";
    let mut item = worst_item(1);
    item.event.process_path = Some(ProcessPath::new(&format!("{canary}/x")));
    let mut snap = worst_item(1).snapshot.unwrap();
    snap.snapshot = Snapshot::new(&format!("{{\"path\":\"{canary}-rule\"}}")).unwrap();
    item.snapshot = Some(snap);
    rig.engine.insert_batch(&[item]).unwrap();
    let find = |needle: &str| {
        let bytes = std::fs::read(rig.location.db_path()).unwrap();
        bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
    };
    assert!(find(canary), "the canary is stored while the row exists");
    rig.engine.evict_chunk().unwrap();
    assert!(
        !find(canary),
        "event and its orphaned snapshot are zeroed on delete"
    );
    assert!(!rig.location.db_path().with_extension("db-journal").exists());
}

#[test]
fn evicting_an_event_removes_its_unreferenced_snapshot_but_keeps_shared_ones() {
    let mut rig = rig(16 * MIB);
    let shared = SnapshotRecord {
        rule_hash: hash_of(9),
        snapshot: Snapshot::new("{\"shared\":1}").unwrap(),
        first_seen_ms: 0,
    };
    let mut a = small_item(1);
    a.event.rule_hash = Some(hash_of(9));
    a.snapshot = Some(shared.clone());
    let mut b = small_item(2);
    b.event.ts_ms = 1_000;
    b.event.rule_hash = Some(hash_of(9));
    rig.engine.insert_batch(&[a, b]).unwrap();
    let count = |rig: &Rig| -> i64 {
        rig.engine
            .connection()
            .query_row("SELECT count(*) FROM rules_seen", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&rig), 1);
    // rows_per_txn may be 2: evict until exactly the first event is gone.
    rig.engine
        .connection()
        .execute("DELETE FROM events WHERE ts_ms = 1000", [])
        .unwrap();
    rig.engine.evict_chunk().unwrap();
    assert_eq!(count(&rig), 0, "no event refers to it any more");
}

// ---- queue ------------------------------------------------------------

#[test]
fn a_full_queue_drops_and_counts_without_blocking() {
    // No writer is draining: the same as a writer stuck in a slow disk.
    let queue = Queue::new(64 * 1024, 100);
    let start = Instant::now();
    let mut dropped = 0;
    for i in 0..10_000 {
        if queue.try_push(small_item(i)).is_err() {
            dropped += 1;
        }
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(queue.len(), 100);
    assert_eq!(dropped, 9_900);
    assert_eq!(queue.dropped(), 9_900);
}

#[test]
fn the_queue_is_bounded_by_bytes_as_well_as_count() {
    let queue = Queue::new(100_000, 4096);
    let mut accepted = 0;
    for i in 0..100 {
        if queue.try_push(worst_item(i)).is_ok() {
            accepted += 1;
        }
    }
    let one = worst_item(0).queue_bytes();
    assert_eq!(accepted, 100_000 / one);
    assert!(queue.dropped() > 0);
}

#[test]
fn pushing_while_the_writer_holds_the_database_does_not_wait() {
    let mut rig = rig(16 * MIB);
    let queue = Arc::new(Queue::with_defaults());
    // The "slow disk": a transaction held open by another connection.
    let blocker = Connection::open(rig.location.db_path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    rig.engine
        .connection()
        .busy_timeout(Duration::from_millis(1500))
        .unwrap();
    let q = queue.clone();
    let writer = std::thread::spawn(move || {
        let _ = rig.engine.run_once(&q, Duration::from_millis(10));
    });
    let start = Instant::now();
    for i in 0..2000 {
        let _ = queue.try_push(small_item(i));
        std::thread::sleep(Duration::from_micros(50));
    }
    let slowest_push = start.elapsed();
    blocker.execute_batch("ROLLBACK;").unwrap();
    writer.join().unwrap();
    // 2000 pushes with 50us sleeps take ~0.1 s; a push waiting on the
    // writer's 1.5 s lock wait would have shown.
    assert!(slowest_push < Duration::from_secs(1), "{slowest_push:?}");
}

#[test]
fn the_writer_thread_drains_the_queue_and_shuts_down_clean() {
    let rig = rig(16 * MIB);
    let location = rig.location.clone();
    let queue = Arc::new(Queue::with_defaults());
    for i in 0..50 {
        queue.try_push(small_item(i)).unwrap();
    }
    let writer = Writer::spawn(rig.engine, queue).unwrap();
    writer.shutdown().unwrap();
    let conn = Connection::open(location.db_path()).unwrap();
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 50);
}

// ---- the proof's invariant, against the real engine --------------------

struct Stat {
    depth: u64,
    min_interior_cells: u64,
    interior_pages: u64,
}

/// Depth and minimum non-root interior fill of every b-tree, from `dbstat`.
fn dbstat(conn: &Connection) -> Stat {
    let mut stat = Stat {
        depth: 0,
        min_interior_cells: u64::MAX,
        interior_pages: 0,
    };
    let mut q = conn
        .prepare("SELECT path, pagetype, ncell FROM dbstat WHERE aggregate = FALSE")
        .unwrap();
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (path, kind, cells) = row.unwrap();
        if kind == "overflow" {
            continue;
        }
        // "/" is a root; "/000+001/" is a child: one segment per level.
        let depth = path.split('/').filter(|s| !s.is_empty()).count() as u64 + 1;
        stat.depth = stat.depth.max(depth);
        if kind == "internal" && depth > 1 {
            stat.interior_pages += 1;
            stat.min_interior_cells = stat.min_interior_cells.min(cells as u64);
        }
    }
    stat
}

#[test]
fn the_fill_and_depth_invariant_holds_under_inserts_and_random_deletes() {
    // Many small rows build deep trees; random deletes (not just the oldest)
    // are the hardest case for fill.
    let mut rig = rig_with(256 * MIB, true);
    let n = rig.engine.budget().rows_per_txn;
    let total = 120_000u64;
    let mut i = 0;
    while i < total {
        let batch: Vec<Item> = (i..(i + n).min(total)).map(small_item).collect();
        rig.engine.insert_batch(&batch).unwrap();
        i += n;
    }
    let conn = rig.engine.connection();
    let check = |label: &str| {
        let s = dbstat(conn);
        println!(
            "{label}: depth {} interior pages {} min cells {}",
            s.depth, s.interior_pages, s.min_interior_cells
        );
        assert!(
            s.depth <= rig.engine.budget().depth,
            "{label}: depth {}",
            s.depth
        );
        assert!(
            s.interior_pages == 0 || s.min_interior_cells + 1 >= MIN_FANOUT,
            "{label}: fanout {}",
            s.min_interior_cells + 1
        );
        s
    };
    let s = check("after inserts");
    assert!(s.interior_pages > 0, "the workload built interior pages");
    for pct in [50, 70, 90] {
        conn.execute(
            "DELETE FROM events WHERE (id * 2654435761) % 100 < ?1",
            [pct],
        )
        .unwrap();
        check(&format!("after deleting ~{pct}%"));
    }
    assert_all_within_budget(&rig);
}

#[test]
fn journals_of_full_batches_on_deep_trees_stay_within_the_bound() {
    let mut rig = rig_with(256 * MIB, true);
    let n = rig.engine.budget().rows_per_txn;
    let mut i = 0;
    while i < 60_000 {
        let batch: Vec<Item> = (i..(i + n).min(60_000)).map(small_item).collect();
        rig.engine.insert_batch(&batch).unwrap();
        i += n;
    }
    for _ in 0..40 {
        rig.engine.evict_chunk().unwrap();
        rig.engine.vacuum_chunk().unwrap();
    }
    assert_all_within_budget(&rig);
}

// ---- review round 2 ---------------------------------------------------

#[test]
fn pages_are_never_spilled_mid_transaction_so_the_journal_has_one_header() {
    // A cache spill syncs the journal mid-transaction, after which SQLite
    // starts a new sector-aligned header: more than `header + 4104 * pages`.
    let mut rig = rig(16 * MIB);
    let conn = rig.engine.connection();
    let spill: i64 = conn
        .query_row("PRAGMA cache_spill", [], |r| r.get(0))
        .unwrap();
    assert_eq!(spill, 0, "cache_spill must be OFF");
    // A cache far smaller than a batch's dirty pages would force spills.
    conn.pragma_update(None, "cache_size", 5).unwrap();
    let n = rig.engine.budget().rows_per_txn;
    for k in 0..30 {
        let batch: Vec<Item> = (0..n).map(|i| worst_item(k * n + i)).collect();
        rig.engine.insert_batch(&batch).unwrap();
    }
    for r in rig.reports.lock().unwrap().iter() {
        let records = (r.journal_bytes - 512) / JOURNAL_RECORD;
        assert_eq!(
            r.journal_bytes,
            512 + records * JOURNAL_RECORD,
            "{r:?}: not one header plus whole records"
        );
    }
    assert_all_within_budget(&rig);
}

#[test]
fn with_spilling_on_the_single_header_assumption_is_false() {
    // Documents why `cache_spill = OFF` is needed: the same workload with
    // spilling enabled writes extra journal headers.
    let mut rig = rig(16 * MIB);
    let conn = rig.engine.connection();
    conn.pragma_update(None, "cache_spill", "ON").unwrap();
    conn.pragma_update(None, "cache_size", 5).unwrap();
    let n = rig.engine.budget().rows_per_txn;
    for k in 0..30 {
        let batch: Vec<Item> = (0..n).map(|i| worst_item(k * n + i)).collect();
        rig.engine.insert_batch(&batch).unwrap();
    }
    let odd = rig
        .reports
        .lock()
        .unwrap()
        .iter()
        .any(|r| (r.journal_bytes - 512) % JOURNAL_RECORD != 0);
    assert!(
        odd,
        "expected a spilled transaction to break the one-header shape"
    );
}

#[test]
fn a_push_never_waits_for_the_lock_the_writer_holds() {
    let queue = Arc::new(Queue::with_defaults());
    let held = queue.clone();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = held.lock_for_test();
        locked_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });
    locked_rx.recv().unwrap();
    // On its own thread with a deadline, so a blocking push fails the test
    // instead of hanging it.
    let pusher = queue.clone();
    let (res_tx, res_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let start = Instant::now();
        let _ = res_tx.send((pusher.try_push(small_item(1)), start.elapsed()));
    });
    let (result, took) = res_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("try_push waited for the lock");
    assert!(took < Duration::from_millis(200), "{took:?}");
    assert_eq!(result, Err(Dropped));
    assert_eq!(queue.dropped(), 1);
    done_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(queue.try_push(small_item(2)).is_ok());
}

#[test]
fn queue_accounting_covers_the_item_and_every_allocation() {
    // Independent of `queue_bytes()`: a floor computed from the type's size
    // and the fields' own capacities.
    for item in [small_item(1), worst_item(2)] {
        let e = &item.event;
        let floor = std::mem::size_of::<Item>()
            + [
                e.process_path.as_ref().map(|v| v.allocated()),
                e.dst_host.as_ref().map(|v| v.allocated()),
                e.protocol.as_ref().map(|v| v.allocated()),
                e.rule_name.as_ref().map(|v| v.allocated()),
                e.rule_class.as_ref().map(|v| v.allocated()),
            ]
            .into_iter()
            .flatten()
            .sum::<usize>()
            + item.snapshot.as_ref().map_or(0, |s| s.snapshot.allocated());
        assert!(
            item.queue_bytes() >= floor,
            "{} < {floor}",
            item.queue_bytes()
        );
    }
    assert!(small_item(1).queue_bytes() >= std::mem::size_of::<Item>());
}

#[test]
fn a_journal_that_cannot_be_measured_is_an_error_not_zero() {
    let mut rig = rig_with(16 * MIB, true);
    // A path whose parent is a regular file: stat fails with ENOTDIR, not
    // NotFound.
    let not_a_dir = rig.location.settings_path();
    std::fs::write(&not_a_dir, b"x").unwrap();
    rig.engine
        .set_journal_path_for_test(not_a_dir.join("journal"));
    let result = rig.engine.insert_batch(&[small_item(1)]);
    assert!(matches!(result, Err(WriteError::Io(_))), "{result:?}");
    let rows: i64 = rig
        .engine
        .connection()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "nothing committed after a failed measurement");
}

#[test]
fn a_failed_batch_is_counted_as_lost_events() {
    let rig = rig_with(16 * MIB, true);
    let mut tiny = *rig.engine.budget();
    tiny.journal_pages = 1; // every real batch trips the budget
    let engine = Engine::new(open_store(&rig.location).unwrap(), &rig.location, tiny).unwrap();
    let queue = Arc::new(Queue::with_defaults());
    for i in 0..2 {
        queue.try_push(small_item(i)).unwrap();
    }
    let writer = Writer::spawn(engine, queue.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while writer.failed_batches() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(writer.failed_batches() >= 1);
    assert_eq!(
        writer.lost_events(),
        2,
        "the popped batch is counted, not forgotten"
    );
    let _ = writer.shutdown();
}

#[test]
fn a_dead_writer_is_visible_to_producers_and_shutdown() {
    let mut rig = rig_with(16 * MIB, true);
    rig.engine.set_observer(Box::new(|_| panic!("writer bug")));
    let queue = Arc::new(Queue::with_defaults());
    queue.try_push(small_item(1)).unwrap();
    let writer = Writer::spawn(rig.engine, queue.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while queue.try_push(small_item(2)).is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        queue.dropped() > 0,
        "producers see the closed queue and count drops"
    );
    assert!(
        writer.shutdown().is_err(),
        "the panic is reported, not discarded"
    );
}

#[test]
fn the_tripwire_leaves_a_record_for_what_commit_adds() {
    // A budget whose journal limit is just above what one real batch has
    // written before COMMIT: within `limit`, but not within `limit - 1 record`.
    let rig = rig_with(16 * MIB, true);
    let mut probe = *rig.engine.budget();
    probe.journal_pages = 100;
    let mut first = Engine::new(open_store(&rig.location).unwrap(), &rig.location, probe).unwrap();
    let seen = Arc::new(Mutex::new(0));
    let sink = seen.clone();
    first.set_observer(Box::new(move |r| *sink.lock().unwrap() = r.journal_bytes));
    first.insert_batch(&[worst_item(1)]).unwrap();
    let measured = *seen.lock().unwrap();
    drop(first);
    let pages = (measured - 512).div_ceil(JOURNAL_RECORD);
    let mut tight = *rig.engine.budget();
    tight.journal_pages = pages;
    assert!(tight.journal_limit_bytes() >= measured);
    assert!(tight.journal_limit_bytes() - JOURNAL_RECORD < measured);
    let mut engine = Engine::new(open_store(&rig.location).unwrap(), &rig.location, tight).unwrap();
    // Same rows again (a new id each): the journal is the same size.
    let result = engine.insert_batch(&[worst_item(2)]);
    assert!(
        matches!(result, Err(WriteError::BudgetExceeded { .. })),
        "{result:?}"
    );
}
