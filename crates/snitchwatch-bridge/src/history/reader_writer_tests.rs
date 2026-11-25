//! The real capture writer against readers reading flat out: reads must not
//! make a commit fail, lose a batch to a `write-failed` gap, or overflow the
//! queue (journal mode DELETE: a reader's lock delays the writer's commit,
//! and the writer gives up after 250 ms).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use snitchwatch_proto::protocol::{Connection, Event, Operator, Rule};

use super::admission::AdmissionConfig;
use super::capture::{Capture, CaptureConfig};
use super::durable::tests::{engine_at, manual_clock};
use super::lifecycle::Generation;
use super::pins::PinRegistry;
use super::reader::*;
use super::reader_registry::CancelToken;
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, Consent, Mode};

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

fn config() -> CaptureConfig {
    let mut c = CaptureConfig::for_budget(&super::budget::Budget::new(16 * 1024 * 1024).unwrap());
    c.admission = AdmissionConfig::standard();
    c.admission.process_rate = 1_000_000;
    c.admission.process_burst = 1_000_000;
    c.admission.global_rate = 1_000_000;
    c.admission.global_burst = 1_000_000;
    c
}

#[test]
fn the_real_writer_commits_every_batch_while_two_readers_read_flat_out() {
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
    let engine = engine_at(&location);
    engine
        .connection()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000) \
             INSERT INTO events (ts_ms, action, answered_by, process_path) \
             SELECT x, 'allow', 'rule', '/usr/bin/old' FROM c;",
        )
        .unwrap();
    let (_clock, read) = manual_clock(10_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let capture = Capture::spawn(engine, registry, config()).unwrap();
    let db = location.db_path();
    let (generation, cancel) = (Generation::default(), CancelToken::default());
    let done = AtomicBool::new(false);
    let reads = AtomicUsize::new(0);
    let stats = std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let ctx = Context {
                    db_path: &db,
                    generation: &generation,
                    cancel: &cancel,
                    now_ms: 1_000_000,
                    live_session_start_ms: None,
                    standing: None,
                    limits: Limits::default(),
                };
                while !done.load(Ordering::SeqCst) {
                    let _ = read_page(
                        &Request {
                            cursor: None,
                            limit: 200,
                        },
                        &ctx,
                    );
                    reads.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        // Every batch is sent while readers are demonstrably mid-flight: wait
        // on observable state (completed reads), never a sleep.
        let wait_for_reads = |n: usize| {
            let target = reads.load(Ordering::SeqCst) + n;
            let give_up = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while reads.load(Ordering::SeqCst) < target {
                assert!(std::time::Instant::now() < give_up, "readers stopped");
                std::thread::yield_now();
            }
        };
        wait_for_reads(4);
        for batch in 0..30 {
            wait_for_reads(2);
            let events: Vec<Event> = (0..20).map(|i| event(batch * 20 + i)).collect();
            let refs: Vec<&Event> = events.iter().collect();
            capture.observe(&refs);
        }
        let stats = capture.shutdown();
        done.store(true, Ordering::SeqCst);
        stats
    });
    assert_eq!(stats.written, 600, "{stats:?}");
    assert_eq!(stats.lost_write, 0, "{stats:?}");
    assert_eq!(stats.dropped_queue, 0, "{stats:?}");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let failed: i64 = conn
        .query_row(
            "SELECT count(*) FROM gaps WHERE cause IN ('write-failed', 'queue-overflow')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failed, 0, "a read made the writer lose a batch");
}
