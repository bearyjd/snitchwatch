//! The control gate's order (PR 10, here with a stub caller) against real
//! reads: advance the generation, close, cancel, wait idle, then unlink.

use std::time::Duration;

use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_registry::{NotAdmitted, ReaderRegistry};

const LONG: Duration = Duration::from_secs(20);

#[test]
fn a_reader_mid_query_when_a_clear_unlinks_the_file_delivers_nothing_after() {
    let fx = Fx::new();
    fx.events(1..=5_000);
    let registry = ReaderRegistry::new();
    let ticket = registry.try_acquire().unwrap();
    let token = ticket.cancel_token();
    let (db, generation, limits) = (&fx.db, &fx.generation, fx.limits);
    let started = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            // The worker owns the ticket, as the real caller's does.
            let _ticket = ticket;
            let ctx = Context {
                db_path: db,
                generation,
                cancel: &token,
                now_ms: 1_000_000,
                live_session_start_ms: None,
                standing: None,
                limits,
            };
            let mut delivered_after_stop = 0;
            loop {
                started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match read_page(
                    &Request {
                        cursor: None,
                        limit: 200,
                    },
                    &ctx,
                ) {
                    Outcome::Page(_) => {
                        if token.is_cancelled() {
                            delivered_after_stop += 1;
                        }
                    }
                    Outcome::Cancelled | Outcome::Superseded => return delivered_after_stop,
                    other => panic!("{other:?}"),
                }
            }
        });
        while started.load(std::sync::atomic::Ordering::SeqCst) < 3 {
            std::thread::yield_now();
        }
        // The stub clear: generation, close, cancel, wait, then unlink.
        fx.generation.advance().unwrap();
        registry.close();
        registry.cancel_all();
        assert!(
            registry.wait_idle(LONG),
            "the reader stopped before the unlink"
        );
        std::fs::remove_file(&fx.db).unwrap();
        assert_eq!(
            worker.join().unwrap(),
            0,
            "a page was delivered after the stop"
        );
    });
    assert_eq!(
        registry.try_acquire().err(),
        Some(NotAdmitted::ControlOperation)
    );
    registry.reopen();
    // After the unlink the reader says there is no history: never an empty page.
    let after = fx.read(None, 10);
    assert!(
        matches!(after, Outcome::Unavailable(ref u) if u.reason == "no history"),
        "{after:?}"
    );
}

#[test]
fn a_worker_that_cannot_stop_makes_the_stub_fail_closed() {
    let registry = ReaderRegistry::new();
    let stuck = registry.try_acquire().unwrap();
    registry.close();
    registry.cancel_all();
    // The reader holds its ticket (a stuck worker): wait_idle says so, and the
    // caller must refuse to unlink.
    assert!(!registry.wait_idle(Duration::from_millis(50)));
    drop(stuck);
    assert!(registry.wait_idle(LONG));
}
