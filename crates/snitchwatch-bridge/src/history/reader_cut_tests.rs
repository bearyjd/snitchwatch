//! A cut that read nothing is never a successful empty page; which cuts a
//! retry can fix; and what the end of a trimmed page still says.

use std::time::Duration;

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_sql::{events_sql, Seek, DUPLICATE_PROBE_CAP};

fn unavailable(outcome: Outcome) -> Unavail {
    match outcome {
        Outcome::Unavailable(u) => u,
        other => panic!("expected unavailable, got {other:?}"),
    }
}

fn locked_writer(fx: &Fx) -> rusqlite::Connection {
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    writer
}

#[test]
fn a_busy_cut_that_read_no_event_is_unavailable_retry_at_the_head_and_on_a_cursor() {
    let fx = Fx::new();
    fx.events(1..=100);
    let first = fx.page(None, 10);
    for cursor in [None, first.next_cursor] {
        let ctx = fx.ctx();
        let request = Request { cursor, limit: 10 };
        let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
        let writer = locked_writer(&fx);
        let loaded = read.load_events();
        writer.execute_batch("ROLLBACK").unwrap();
        let Err(outcome) = loaded else {
            panic!("a read that got nothing went on")
        };
        let u = unavailable(outcome);
        assert!(u.retry, "{}", u.reason);
        assert_ne!(u.kind, UnavailKind::TooExpensive);
    }
}

/// Prompts and pings that never match each other, all at one instant.
fn dense_store(fx: &Fx) {
    fx.conn()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 60000) \
             INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
             SELECT 1000, 1000, 'replied', '/a', 'allow', 'gui' FROM c;",
        )
        .unwrap();
}

#[test]
fn a_time_cut_before_the_first_event_of_a_page_of_one_is_too_slow_not_retryable() {
    let mut fx = Fx::new();
    dense_store(&fx);
    fx.limits.statement_budget = Duration::ZERO;
    let mut cut_seen = false;
    for _ in 0..5 {
        match fx.read(None, 1) {
            Outcome::Page(page) => assert!(!page.events.is_empty(), "{page:?}"),
            Outcome::Unavailable(u) => {
                cut_seen = true;
                assert!(!u.retry, "a retry loop has no bound: {}", u.reason);
                assert_eq!(u.kind, UnavailKind::TooExpensive, "{}", u.reason);
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(
        cut_seen,
        "the zero budget never cut a statement: not tested"
    );
}

#[test]
fn only_a_scan_cap_cut_before_the_first_event_is_too_expensive() {
    let mut fx = Fx::new();
    fx.events(1..=10);
    fx.limits.scan_rows_max = 0;
    let u = unavailable(fx.read(None, 10));
    assert_eq!(u.kind, UnavailKind::TooExpensive, "{}", u.reason);
    assert!(!u.retry);
}

#[test]
fn trimming_the_ledger_never_turns_a_retention_floor_into_the_oldest_recorded() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1_000);
    fx.now_ms = 60_000;
    fx.session(1_000);
    fx.events([50_000]);
    // The oldest ledger row is the eviction marker; 120 newer disjoint gaps
    // make the window carry more rows than a page may.
    fx.gap(2_000, 2_500, GapCause::Evicted, 5);
    for i in 0..120 {
        fx.gap(3_000 + i * 10, 3_005 + i * 10, GapCause::WriteFailed, 1);
    }
    let page = fx.page(None, 200);
    assert_eq!(page.partial, Some(Partial::Gaps), "the ledger was trimmed");
    assert_eq!(page.end, End::RetentionFloor);
}

#[test]
fn a_ping_that_cannot_pair_is_not_flagged_past_the_probe_cap() {
    // A migrated row: a decision time but no prompt outcome. It is no ping and
    // no prompt, so no pair can include it, whatever is near.
    let fx = Fx::new();
    fx.conn()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 300) \
             INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
             SELECT 1000, 1000, 'replied', '/a', 'allow', 'gui' FROM c; \
             INSERT INTO events (ts_ms, decided_ts_ms, process_path, action, answered_by) \
             VALUES (1000, 1000, '/b', 'allow', 'rule');",
        )
        .unwrap();
    let row = fx.page(None, 1).events.remove(0);
    assert_eq!(row.prompt_outcome, None);
    assert!(
        !row.possible_duplicate,
        "it cannot be a duplicate of anything"
    );
}

#[test]
fn a_valid_eight_kib_page_limit_holds_an_event_that_fits() {
    let mut fx = Fx::new();
    let path = "/".to_string() + &"a".repeat(4095);
    fx.conn()
        .execute(
            "INSERT INTO events (ts_ms, process_path, action, answered_by) \
             VALUES (1000, ?1, 'allow', 'rule')",
            [&path],
        )
        .unwrap();
    fx.limits.page_bytes = 8 * 1024;
    let page = fx.page(None, 10);
    assert_eq!(page.events.len(), 1);
    assert!(page.bytes <= 8 * 1024, "{}", page.bytes);
    let json = serde_json::to_vec(&page).unwrap();
    assert!(json.len() <= 8 * 1024, "{} bytes really", json.len());
}

/// What the page's duplicate lookups cost: SQLite's own count of virtual
/// machine steps for the head statement of `limit` rows.
fn head_steps(fx: &Fx, limit: usize, cap: usize) -> i32 {
    let conn = fx.conn();
    let mut stmt = conn.prepare(&events_sql(Seek::Head, cap)).unwrap();
    let rows = stmt
        .query_map([i64::try_from(limit).unwrap()], |_| Ok(()))
        .unwrap()
        .count();
    assert_eq!(rows, limit);
    stmt.get_status(rusqlite::StatementStatus::VmStep)
}

#[test]
fn two_hundred_pings_over_257_prompts_stay_within_the_page_bound() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 257) \
             INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
             SELECT 1000, 1000, 'replied', '/a', 'allow', 'gui' FROM c; \
             WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 200) \
             INSERT INTO events (ts_ms, process_path, action, answered_by) \
             SELECT 2000, '/b', 'allow', 'rule' FROM c;",
        )
        .unwrap();
    // One cap for every page: a page examines at most its rows times
    // (cap + 1) candidates, and the statement stays under a virtual machine
    // step ceiling. Counts only: wall-clock time depends on the machine.
    let limits = Limits::default();
    assert!(
        limits.events_per_page * (DUPLICATE_PROBE_CAP + 1) <= 52_000,
        "candidates a page may examine"
    );
    let page = fx.page(None, 200);
    assert!(!page.events.is_empty(), "progress");
    let steps = head_steps(&fx, 200, DUPLICATE_PROBE_CAP);
    assert!(steps < 1_000_000, "{steps} virtual machine steps");
}

#[test]
fn the_reserved_header_covers_the_worst_page_without_rows() {
    let mut fx = Fx::new();
    fx.events([1_000]);
    fx.live_session_start_ms = Some(10);
    let mut page = fx.page(None, 10);
    page.events.clear();
    page.coverage.clear();
    page.sessions.clear();
    page.gaps.clear();
    let json = serde_json::to_vec(&page).unwrap().len();
    // Twelve integers, each up to 20 digits where this page has a few.
    let worst = json + 12 * 18;
    assert!(
        worst <= super::reader_size::HEADER_BYTES,
        "{worst} bytes for a header of {}",
        super::reader_size::HEADER_BYTES
    );
}
