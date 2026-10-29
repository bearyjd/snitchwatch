//! The writer under worst-case rows: the full-journal disk bound (S2, S3),
//! shrink on free (S4), canary scan (S5) and a non-blocking queue.
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
/// The disk-bound tests use the production setting (FULL), which changes how the
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

fn assert_all_within_budget(rig: &Rig) {
    let budget = *rig.engine.budget();
    for r in rig.reports.lock().unwrap().iter() {
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

// ---- S2: the full-journal bound ----------------------------------------

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
fn worst_case_churn_keeps_every_journal_within_the_full_journal_limit() {
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
fn main_file_plus_peak_journal_plus_settings_stays_within_b() {
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
    let total_peak = main_peak + journal_peak + settings_peak;
    println!("main {main_peak} + journal {journal_peak} + settings {settings_peak} = {total_peak} of {total}");
    assert!(total_peak <= total);
}

/// Insert worst-case rows, no eviction, until the file refuses to grow.
fn fill_to_limit(rig: &mut Rig) {
    let n = rig.engine.budget().rows_per_txn;
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
}

#[test]
fn the_main_file_never_exceeds_half_of_b_when_filled_to_the_limit() {
    // `max_page_count` is the enforcement, not the eviction policy.
    let total = 16 * MIB;
    let mut rig = rig_with(total, true);
    let budget = *rig.engine.budget();
    fill_to_limit(&mut rig);
    let len = file_len(&rig.location.db_path());
    assert!(len <= total / 2, "{len} B is over half of {total}");
    assert!(len <= budget.max_page_count * PAGE);
    assert!(len >= budget.main_bytes / 100 * 95, "the limit was reached");
}

#[test]
fn main_plus_journal_stays_within_b_when_a_transaction_journals_every_page() {
    // The full-journal case: fill the file, then rewrite all of it in one
    // transaction (deleting every row zeroes every page: `secure_delete`).
    let total = 16 * MIB;
    let mut rig = rig(total);
    let budget = *rig.engine.budget();
    fill_to_limit(&mut rig);
    let journal = rig.location.db_path().with_file_name("history.db-journal");
    let conn = rig.engine.connection();
    conn.execute_batch("BEGIN IMMEDIATE; DELETE FROM events; DELETE FROM rules_seen;")
        .unwrap();
    let main = file_len(&rig.location.db_path());
    let journaled = file_len(&journal);
    conn.execute_batch("ROLLBACK").unwrap();
    println!("main {main} + journal {journaled} of {total}");
    assert!(
        journaled >= main / 10 * 8,
        "the workload must journal most of the file ({journaled} of {main})"
    );
    assert!(journaled <= budget.journal_limit_bytes());
    assert!(
        main + journaled + budget.settings_bytes <= total,
        "main {main} + journal {journaled} + settings is over {total}"
    );
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
    let queue = Queue::new(4 * MIB as usize, 100);
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
    let n = 4;
    for k in 0..30 {
        assert!(rig.engine.make_room().unwrap());
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
    let n = 4;
    for k in 0..30 {
        assert!(rig.engine.make_room().unwrap());
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

// ---- review round 3 ---------------------------------------------------

#[test]
fn a_database_over_a_lowered_budget_refuses_to_start_and_loses_nothing() {
    let big = rig_with(64 * MIB, true);
    let mut big = big;
    // Grow the file past the 16 MiB budget's main-file cap (8 MiB).
    let mut i = 0;
    while file_len(&big.location.db_path()) < 9 * MIB {
        let batch: Vec<Item> = (i..i + 32).map(worst_item).collect();
        big.engine.insert_batch(&batch).unwrap();
        i += 32;
    }
    let location = big.location.clone();
    let rows = i as i64;
    drop(big.engine);
    let store = open_store(&location).unwrap();
    let small = Budget::new(16 * MIB).unwrap();
    let err = match Engine::new(store, &location, small) {
        Ok(_) => panic!("an over-cap database must not get a writer"),
        Err(e) => e,
    };
    assert!(matches!(err, WriteError::OverBudget { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("pages") && text.contains("budget"), "{text}");
    let count: i64 = Connection::open(location.db_path())
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, rows, "nothing was deleted");
}

#[test]
fn a_connection_switched_to_wal_before_spawn_is_refused() {
    let rig = rig_with(16 * MIB, true);
    let mode: String = rig
        .engine
        .connection()
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // A reader holding a WAL snapshot stops the mode from being reset.
    let reader = Connection::open(rig.location.db_path()).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM events;")
        .unwrap();
    let result = Writer::spawn(rig.engine, Arc::new(Queue::with_defaults()));
    assert!(result.is_err(), "WAL must be refused at spawn");
    drop(reader);
}

#[test]
fn spawn_puts_a_switched_connection_back_in_delete_mode() {
    let rig = rig_with(16 * MIB, true);
    let _: String = rig
        .engine
        .connection()
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    let location = rig.location.clone();
    let writer = Writer::spawn(rig.engine, Arc::new(Queue::with_defaults())).unwrap();
    let mode: String = Connection::open(location.db_path())
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    writer.shutdown().unwrap();
}

#[test]
fn stale_settings_temp_files_are_removed_when_the_writer_opens() {
    let rig = rig_with(16 * MIB, true);
    let settings = rig.location.settings_path();
    let dir = settings.parent().unwrap().to_path_buf();
    // A pid far above pid_max: no such process.
    let stale: Vec<_> = (0..20)
        .map(|k| dir.join(format!(".settings.json.4000000000.{k:016x}.tmp")))
        .collect();
    for p in &stale {
        std::fs::write(p, vec![b' '; 4096]).unwrap();
    }
    let me = std::process::id();
    let active = dir.join(format!(".settings.json.{me}.{:016x}.tmp", 7));
    let live_other = dir.join(".settings.json.1.0000000000000001.tmp");
    let not_ours = [
        ".settings.json.backup.tmp",
        ".settings.json.4000000000.XYZ.tmp",
        ".settings.json.4000000000.00000000000000001.tmp",
        ".settings.json.4000000000.0000000000000001.tmp.bak",
        "settings.json.4000000000.0000000000000001.tmp",
        "keep.txt",
    ]
    .map(|n| dir.join(n));
    for p in [&active, &live_other].into_iter().chain(&not_ours) {
        std::fs::write(p, b"x").unwrap();
    }
    let link = dir.join(".settings.json.4000000000.00000000000000aa.tmp");
    std::os::unix::fs::symlink(other_target(&dir), &link).unwrap();
    let before = std::fs::read(&settings).unwrap();
    let _again = Engine::new(
        open_store(&rig.location).unwrap(),
        &rig.location,
        Budget::new(16 * MIB).unwrap(),
    )
    .unwrap();
    assert!(stale.iter().all(|p| !p.exists()), "stale temp files remain");
    assert!(
        active.exists(),
        "this process's temp file may be a save in flight"
    );
    assert!(
        live_other.exists(),
        "a live process's temp file is left alone"
    );
    for p in &not_ours {
        assert!(p.exists(), "{p:?} is not a generated temp name");
    }
    assert!(link.symlink_metadata().is_ok(), "a link is never removed");
    assert!(other_target(&dir).exists(), "and never followed");
    assert_eq!(std::fs::read(&settings).unwrap(), before);
}

fn other_target(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("link-target.txt");
    std::fs::write(&p, b"t").unwrap();
    p
}

#[test]
fn a_save_in_flight_keeps_its_temp_file_when_a_writer_opens() {
    let rig = rig_with(16 * MIB, true);
    let location = rig.location.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let current = super::settings::load(&location).unwrap().unwrap();
    let loc = location.clone();
    let saver = std::thread::spawn(move || {
        let mut n = 0;
        while !flag.load(std::sync::atomic::Ordering::Relaxed) && n < 100_000 {
            super::settings::save(&loc, &current).expect("save raced with the sweep");
            n += 1;
        }
    });
    for _ in 0..300 {
        let _e = Engine::new(
            open_store(&location).unwrap(),
            &location,
            Budget::new(16 * MIB).unwrap(),
        )
        .unwrap();
        super::settings::load(&location).expect("load raced with the save");
    }
    for _ in 0..20_000 {
        super::settings::load(&location).expect("load raced with the save");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    saver.join().unwrap();
}

#[test]
fn a_mode_sqlite_kept_is_refused() {
    for mode in ["wal", "memory", "off", "truncate"] {
        assert!(
            matches!(
                require_delete_mode(mode.to_string()),
                Err(WriteError::JournalMode(_))
            ),
            "{mode}"
        );
    }
    assert!(require_delete_mode("delete".into()).is_ok());
    assert!(require_delete_mode("DELETE".into()).is_ok());
}

#[test]
fn a_tiny_byte_limit_covers_the_deques_real_first_allocation() {
    let item = small_item(1);
    let limit = item.queue_bytes();
    let queue = Queue::new(limit, 100);
    queue.try_push(item).unwrap();
    let (_, cap) = queue.retained_for_test();
    assert!(
        cap * std::mem::size_of::<Item>() <= limit,
        "retained {cap} slots"
    );
}

#[test]
fn a_drained_queue_gives_its_backing_allocation_back() {
    let queue = Queue::new(64 * MIB as usize, 4096);
    for i in 0..3000 {
        queue.try_push(small_item(i)).unwrap();
    }
    loop {
        let (len, cap) = queue.retained_for_test();
        assert!(
            cap <= 2 * len.max(1) + 4 || len == 3000,
            "len {len} cap {cap}"
        );
        if queue.pop_batch(64, Duration::ZERO).is_empty() {
            break;
        }
    }
    let (len, cap) = queue.retained_for_test();
    assert_eq!(len, 0);
    assert!(cap <= 4, "an empty queue retains {cap} slots");
}

#[test]
fn dropping_a_writer_without_shutdown_stops_and_joins_the_worker() {
    let rig = rig_with(16 * MIB, true);
    let location = rig.location.clone();
    let queue = Arc::new(Queue::with_defaults());
    for i in 0..5 {
        queue.try_push(small_item(i)).unwrap();
    }
    let writer = Writer::spawn(rig.engine, queue.clone()).unwrap();
    drop(writer);
    assert!(queue.try_push(small_item(99)).is_err(), "queue is closed");
    let rows: i64 = Connection::open(location.db_path())
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        rows, 5,
        "queued items were written before the join returned"
    );
}

#[test]
fn reading_a_file_while_it_is_replaced_never_reports_a_hard_link() {
    // A save renames over the file; a reader that opened the old inode sees
    // zero links. That is a replacement, not a hard link: it is retried.
    let rig = rig_with(16 * MIB, true);
    let path = rig.location.settings_path();
    let saver_path = path.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let saver = std::thread::spawn(move || {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            crate::state_file::write(&saver_path, b"{}").unwrap();
        }
    });
    for _ in 0..3000 {
        let read = crate::state_file::read(&path, 4096);
        assert!(read.is_ok(), "{read:?}");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    saver.join().unwrap();
}
