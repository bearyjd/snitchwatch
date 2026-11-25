//! A page always moves the client forward, and the reader's waits and
//! cancels are the ones its limits say.

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;

/// A store with one event and more gap rows at its instant than a page may
/// carry, so the window around it cannot fit.
fn gap_heavy(fx: &mut Fx, events: &[i64], at: i64, rows: usize) {
    fx.live_session_start_ms = Some(at - 50);
    fx.now_ms = at + 50;
    fx.session(at - 50);
    fx.events(events.iter().copied());
    let causes = [
        GapCause::UpstreamLoss,
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
    ];
    for i in 0..rows {
        fx.gap(at, at, causes[i % 3], 1);
    }
}

#[test]
fn a_gap_heavy_instant_does_not_hide_the_event_beside_it() {
    let mut fx = Fx::new();
    gap_heavy(&mut fx, &[1_050], 1_050, 150);
    let page = fx.page(None, 200);
    assert_eq!(page.events.len(), 1, "the event is delivered, not dropped");
    assert_eq!(page.partial, Some(Partial::Gaps));
    assert!(page.gaps.len() <= 100);
    assert!(page.bytes <= fx.limits.page_bytes);
}

#[test]
fn paging_a_gap_heavy_store_reaches_every_event() {
    let mut fx = Fx::new();
    gap_heavy(&mut fx, &[900, 950, 1_050, 1_040], 1_050, 150);
    let pages = fx.all_pages(200);
    let mut ids: Vec<i64> = pages
        .iter()
        .flat_map(|p| p.events.iter().map(|e| e.id))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4, "every event reached once");
}

#[test]
fn a_gap_heavy_head_still_pages_event_by_event() {
    let mut fx = Fx::new();
    gap_heavy(&mut fx, &[1_050, 1_050, 1_050], 1_050, 150);
    let pages = fx.all_pages(1);
    assert_eq!(pages.iter().map(|p| p.events.len()).sum::<usize>(), 3);
}

/// A writer that holds the file for `hold` after the reader starts, then
/// commits: what a real writer's commit window looks like to a reader.
fn writer_commit_for(fx: &Fx, hold: std::time::Duration) -> std::thread::JoinHandle<()> {
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(std::time::Duration::ZERO).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        writer.execute_batch("COMMIT").unwrap();
    })
}

#[test]
fn a_writers_short_commit_is_waited_out_not_reported_busy() {
    let mut fx = Fx::new();
    fx.events(1..=50);
    fx.limits.busy_wait = std::time::Duration::from_secs(5);
    let writer = writer_commit_for(&fx, std::time::Duration::from_millis(80));
    let page = fx.page(None, 10);
    writer.join().unwrap();
    assert_eq!(page.events.len(), 10, "waited for the commit, then read");
    assert_eq!(page.partial, None);
}

#[test]
fn a_commit_that_outlasts_the_busy_wait_is_unavailable_retry() {
    let mut fx = Fx::new();
    fx.events(1..=50);
    fx.limits.busy_wait = std::time::Duration::from_millis(10);
    let writer = writer_commit_for(&fx, std::time::Duration::from_millis(400));
    let outcome = fx.read(None, 10);
    writer.join().unwrap();
    assert!(
        matches!(outcome, Outcome::Unavailable(ref u) if u.retry),
        "{outcome:?}"
    );
}

/// Prompts 16 ms apart, so each has about 250 events within two seconds: under the duplicate lookup's cap, so it runs in full for every
/// returned row, and a page of them is one long statement.
fn slow_statement_store(fx: &Fx, prompts: u32) {
    let sql = format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {prompts}) \
         INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
         SELECT x * 16, x * 16, 'replied', '/a', 'allow', 'gui' FROM c;"
    );
    fx.conn().execute_batch(&sql).unwrap();
}

#[test]
fn a_cancel_stops_a_slow_statement_in_the_reader_itself() {
    let mut fx = Fx::new();
    slow_statement_store(&fx, 60_000);
    // Neither the statement budget nor the deadline may be what ends it.
    fx.limits.statement_budget = std::time::Duration::from_secs(600);
    fx.limits.query_deadline = std::time::Duration::from_secs(600);
    fx.limits.events_per_page = 60_000;
    fx.limits.scan_window = 60_000;
    fx.limits.scan_rows_max = 60_000;
    let started = std::time::Instant::now();
    let outcome = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(150));
            fx.cancel.cancel();
        });
        fx.read(None, 60_000)
    });
    assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
    assert!(
        // Left alone the statement runs for many seconds; the stage-boundary
        // check alone would only notice after it.
        started.elapsed() < std::time::Duration::from_secs(3),
        "stopped promptly: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_cancel_during_the_file_check_is_cancelled_not_unavailable() {
    let mut fx = Fx::new();
    fx.events(1..=10);
    // The writer holds the file, so opening waits out the busy wait and then
    // fails: that failure is the cancel's doing if one landed meanwhile.
    fx.limits.busy_wait = std::time::Duration::from_millis(600);
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(std::time::Duration::ZERO).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let outcome = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            fx.cancel.cancel();
        });
        fx.read(None, 10)
    });
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(matches!(outcome, Outcome::Cancelled), "{outcome:?}");
}
