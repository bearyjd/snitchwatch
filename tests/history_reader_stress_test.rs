//! History PR 9: the reader against a real writer and a stuck-lock probe, with
//! threads reading flat out. These live in their own test binary so they
//! never share a process (and its cores) with the library's sleep-based
//! timing tests; inside the binary they run one at a time.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use snitchwatch_bridge::history::budget::Budget;
use snitchwatch_bridge::history::capture::{Capture, CaptureConfig};
use snitchwatch_bridge::history::lifecycle::Generation;
use snitchwatch_bridge::history::pins::PinRegistry;
use snitchwatch_bridge::history::reader::{read_page, Context, Limits, Outcome, Request};
use snitchwatch_bridge::history::reader_registry::{CancelToken, NotAdmitted, ReaderRegistry};
use snitchwatch_bridge::history::writer::Engine;
use snitchwatch_bridge::history::{enable, locate, open_store, Consent, Location, Mode};
use snitchwatch_bridge::sqlite_file::open_owner_only;
use snitchwatch_proto::protocol::{Connection, Event, Operator, Rule};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner())
}

const BULK: &str =
    "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000) \
    INSERT INTO events (ts_ms, action, answered_by, process_path) \
    SELECT 1000 + x, 'allow', 'rule', '/usr/bin/old' FROM c;";

struct Store {
    _dir: tempfile::TempDir,
    location: Location,
    engine: Engine,
}

/// An enabled history store with 20,000 events, and the writer's engine on it.
fn store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().canonicalize().unwrap().join("state");
    std::fs::create_dir(&state).unwrap();
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
    let engine = Engine::new(store, &location, Budget::new(16 * 1024 * 1024).unwrap()).unwrap();
    engine
        .connection()
        .pragma_update(None, "synchronous", "OFF")
        .unwrap();
    engine.connection().execute_batch(BULK).unwrap();
    Store {
        _dir: dir,
        location,
        engine,
    }
}

fn ctx<'a>(
    db: &'a std::path::Path,
    generation: &'a Generation,
    cancel: &'a CancelToken,
) -> Context<'a> {
    Context {
        db_path: db,
        generation,
        cancel,
        now_ms: 1_000_000,
        live_session_start_ms: None,
        standing: None,
        limits: Limits::default(),
    }
}

/// Two threads reading pages until `done`, counting delivered pages.
fn spawn_readers<'s>(
    scope: &'s std::thread::Scope<'s, '_>,
    db: &'s std::path::Path,
    generation: &'s Generation,
    cancel: &'s CancelToken,
    done: &'s AtomicBool,
    reads: &'s AtomicUsize,
) {
    for _ in 0..2 {
        scope.spawn(move || {
            let ctx = ctx(db, generation, cancel);
            while !done.load(Ordering::SeqCst) {
                let outcome = read_page(
                    &Request {
                        cursor: None,
                        limit: 200,
                    },
                    &ctx,
                );
                match outcome {
                    // Only a delivered page is a read: a reader that cannot
                    // open or read must fail these tests, not satisfy them.
                    Outcome::Page(_) => {
                        reads.fetch_add(1, Ordering::SeqCst);
                    }
                    // The probe's exclusive lock may land on a reader.
                    Outcome::Unavailable(ref u) if u.retry => {}
                    other => panic!("a reader got {other:?}"),
                }
            }
        });
    }
}

/// Waits (on completed reads, never a sleep) until `n` more have finished.
fn wait_for_reads(reads: &AtomicUsize, n: usize) {
    let target = reads.load(Ordering::SeqCst) + n;
    let give_up = Instant::now() + Duration::from_secs(60);
    while reads.load(Ordering::SeqCst) < target {
        assert!(Instant::now() < give_up, "readers stopped");
        std::thread::yield_now();
    }
}

fn event(i: i64) -> Event {
    Event {
        time: String::new(),
        connection: Some(Connection {
            protocol: "tcp".into(),
            src_ip: "192.168.1.10".into(),
            src_port: 51544,
            dst_ip: "140.82.121.4".into(),
            dst_host: "github.com".into(),
            dst_port: 443,
            user_id: 1000,
            process_id: 4242,
            process_path: format!("/usr/bin/p{}", i % 50),
            ..Connection::default()
        }),
        rule: Some(Rule {
            name: "allow-web".into(),
            action: "allow".into(),
            duration: "always".into(),
            enabled: true,
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "process.path".into(),
                data: "/usr/bin/curl".into(),
                ..Operator::default()
            }),
            ..Rule::default()
        }),
        unixnano: (10_000 + i) * 1_000_000,
    }
}

/// The real capture writer commits every batch while two readers read flat
/// out. (The writer retries a busy commit, so this alone does not prove the
/// readers hold no lock: the probe below does.)
#[test]
fn the_real_writer_commits_every_batch_while_two_readers_read_flat_out() {
    let _one = one_at_a_time();
    let s = store();
    let db = s.location.db_path();
    let budget = Budget::new(16 * 1024 * 1024).unwrap();
    let mut config = CaptureConfig::for_budget(&budget);
    config.admission.process_rate = 1_000_000;
    config.admission.process_burst = 1_000_000;
    config.admission.global_rate = 1_000_000;
    config.admission.global_burst = 1_000_000;
    let capture = Capture::spawn(s.engine, Arc::new(PinRegistry::new()), config).unwrap();
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let (done, reads) = (AtomicBool::new(false), AtomicUsize::new(0));
    let stats = std::thread::scope(|scope| {
        spawn_readers(scope, &db, &generation, &cancel, &done, &reads);
        wait_for_reads(&reads, 4);
        for batch in 0..30 {
            wait_for_reads(&reads, 2);
            let events: Vec<Event> = (0..20).map(|i| event(batch * 20 + i)).collect();
            let refs: Vec<&Event> = events.iter().collect();
            capture.observe(&refs);
        }
        let stats = capture.shutdown();
        done.store(true, Ordering::SeqCst);
        stats
    });
    assert_eq!(stats.written, 600, "{stats:?}");
    assert_eq!((stats.lost_write, stats.dropped_queue), (0, 0), "{stats:?}");
    let conn = open_owner_only(&db).unwrap();
    let failed: i64 = conn
        .query_row(
            "SELECT count(*) FROM gaps WHERE cause IN ('write-failed', 'queue-overflow')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failed, 0, "a read made the writer lose a batch");
}

/// A writer with the history writer's 250 ms busy timeout commits 60 times,
/// each with readers demonstrably mid-flight: none may fail.
#[test]
fn commits_succeed_while_two_readers_read_flat_out() {
    let _one = one_at_a_time();
    let s = store();
    let db = s.location.db_path();
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let (done, reads) = (AtomicBool::new(false), AtomicUsize::new(0));
    let failures = std::thread::scope(|scope| {
        spawn_readers(scope, &db, &generation, &cancel, &done, &reads);
        let writer = open_owner_only(&db).unwrap();
        writer.busy_timeout(Duration::from_millis(250)).unwrap();
        writer.pragma_update(None, "synchronous", "OFF").unwrap();
        let failed = (0..60)
            .filter(|i| {
                wait_for_reads(&reads, 2);
                writer
                    .execute(
                        "INSERT INTO events (ts_ms, action, answered_by) VALUES (?1, 'allow', 'rule')",
                        [10_000 + i],
                    )
                    .is_err()
            })
            .count();
        done.store(true, Ordering::SeqCst);
        failed
    });
    assert_eq!(failures, 0, "a reader made a commit give up");
}

/// How long the database stayed locked against a writer, in one unbroken
/// stretch, while two readers read flat out: probed by a writer that tries
/// `BEGIN EXCLUSIVE` with no wait, over and over. Under the writer's busy
/// timeout, by a wide margin.
#[test]
fn no_reader_lock_outlasts_the_writers_busy_timeout() {
    let _one = one_at_a_time();
    let s = store();
    let db = s.location.db_path();
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let (done, reads) = (AtomicBool::new(false), AtomicUsize::new(0));
    let longest = std::thread::scope(|scope| {
        spawn_readers(scope, &db, &generation, &cancel, &done, &reads);
        let probe = open_owner_only(&db).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        let (mut longest, mut since) = (Duration::ZERO, None::<Instant>);
        let give_up = Instant::now() + Duration::from_secs(60);
        while reads.load(Ordering::SeqCst) < 40 {
            assert!(Instant::now() < give_up, "the readers delivered no pages");
            match probe.execute_batch("BEGIN EXCLUSIVE; COMMIT;") {
                Ok(()) => since = None,
                Err(_) => {
                    let start = *since.get_or_insert_with(Instant::now);
                    longest = longest.max(start.elapsed());
                }
            }
        }
        done.store(true, Ordering::SeqCst);
        longest
    });
    assert!(
        longest < Duration::from_millis(250),
        "the database stayed locked for {longest:?} in one stretch"
    );
}

/// The control gate's order (PR 10, with a stub caller) against a real read:
/// advance the generation, close, cancel, wait idle, then unlink. The reader
/// stops before the unlink and delivers nothing after the stop. `bump` leaves
/// the generation alone, so the stop is the cancel's doing alone.
fn quiesce_then_unlink(bump: bool) {
    let s = store();
    let db = s.location.db_path();
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let generation = Generation::default();
    let started = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            // The worker owns the ticket, as the real caller's does.
            let _ticket = ticket;
            let ctx = ctx(&db, &generation, &token);
            let mut delivered_after_stop = 0;
            loop {
                started.fetch_add(1, Ordering::SeqCst);
                match read_page(
                    &Request {
                        cursor: None,
                        limit: 200,
                    },
                    &ctx,
                ) {
                    Outcome::Page(_) if token.is_cancelled() => delivered_after_stop += 1,
                    Outcome::Page(_) => {}
                    Outcome::Cancelled => return delivered_after_stop,
                    Outcome::Superseded if bump => return delivered_after_stop,
                    other => panic!("{other:?}"),
                }
            }
        });
        wait_for_reads(&started, 3);
        if bump {
            generation.advance().unwrap();
        }
        let guard = registry.quiesce(Duration::from_secs(20));
        assert!(guard.is_ok(), "the reader stopped before the unlink");
        // Held across the unlink, as the control gate holds it.
        let guard = guard.ok();
        std::fs::remove_file(&db).unwrap();
        assert_eq!(
            worker.join().unwrap(),
            0,
            "a page was delivered after the stop"
        );
        assert_eq!(
            registry.try_acquire().err(),
            Some(NotAdmitted::ControlOperation)
        );
        drop(guard);
    });
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let after = read_page(
        &Request {
            cursor: None,
            limit: 10,
        },
        &ctx(&db, &generation, &cancel),
    );
    assert!(
        matches!(after, Outcome::Unavailable(ref u) if u.reason == "no history"),
        "{after:?}"
    );
}

#[test]
fn a_reader_mid_query_when_a_clear_unlinks_the_file_delivers_nothing_after() {
    let _one = one_at_a_time();
    quiesce_then_unlink(true);
}

#[test]
fn a_cancel_alone_stops_a_reader_before_the_unlink() {
    let _one = one_at_a_time();
    quiesce_then_unlink(false);
}
