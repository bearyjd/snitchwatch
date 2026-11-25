//! Paging: the plan, ties, and stability under concurrent writes and
//! eviction. Coverage across pages.

use std::collections::BTreeSet;

use rusqlite::StatementStatus;

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_sql::{events_sql, Seek};
use super::retention_support::evict_before;
use super::session::SessionEnd;

fn ids(pages: &[Page]) -> Vec<i64> {
    pages
        .iter()
        .flat_map(|p| p.events.iter().map(|e| e.id))
        .collect()
}

fn assert_no_duplicates(ids: &[i64]) {
    let unique: BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "an event was returned twice");
}

fn all_ids(fx: &Fx) -> BTreeSet<i64> {
    fx.conn()
        .prepare("SELECT id FROM events")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn plan(conn: &rusqlite::Connection, sql: &str, params: &[i64]) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows
}

#[test]
fn every_page_statement_uses_the_time_index_with_no_sort_and_no_table_scan() {
    let fx = Fx::new();
    for (seek, params) in [
        (Seek::Head, vec![200]),
        (Seek::Ties, vec![5, 5, 200]),
        (Seek::Older, vec![5, 200]),
    ] {
        let lines = plan(fx.conn(), &events_sql(seek), &params);
        let joined = lines.join(" | ");
        assert!(joined.contains("events_ts"), "{seek:?}: {joined}");
        assert!(!joined.contains("TEMP B-TREE"), "{seek:?} sorts: {joined}");
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("SCAN events") && !l.contains("INDEX")),
            "{seek:?} scans the table: {joined}"
        );
    }
    // The tie statement is a range on the index itself, not a filter over it.
    let ties = plan(fx.conn(), &events_sql(Seek::Ties), &[5, 5, 200]).join(" | ");
    assert!(
        ties.contains("ts_ms=?") && ties.contains("rowid<?"),
        "{ties}"
    );
}

fn steps(conn: &rusqlite::Connection, cursor: Option<(i64, i64)>) -> i32 {
    let mut total = 0;
    let mut got = 0;
    let statements: Vec<(Seek, Vec<i64>)> = match cursor {
        Some((ts, id)) => vec![
            (Seek::Ties, vec![ts, id, 200]),
            (Seek::Older, vec![ts, 200]),
        ],
        None => vec![(Seek::Head, vec![200])],
    };
    for (seek, params) in statements {
        let mut stmt = conn.prepare(&events_sql(seek)).unwrap();
        let mut rows = stmt.query(rusqlite::params_from_iter(&params)).unwrap();
        while rows.next().unwrap().is_some() {
            got += 1;
        }
        drop(rows);
        total += stmt.get_status(StatementStatus::VmStep);
        if got >= 200 {
            break;
        }
    }
    assert_eq!(got, 200);
    total
}

#[test]
fn fifty_thousand_ties_cost_the_same_at_every_depth() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 50000) \
             INSERT INTO events (ts_ms, action, answered_by) SELECT 0, 'allow', 'rule' FROM c;",
        )
        .unwrap();
    let head = steps(fx.conn(), None);
    let deep = steps(fx.conn(), Some((0, 40_000)));
    let deepest = steps(fx.conn(), Some((0, 300)));
    // A seek that walked past the ties would cost tens of thousands of steps
    // more at depth.
    assert!(deep <= head * 2, "head {head}, deep {deep}");
    assert!(deepest <= head * 2, "head {head}, deepest {deepest}");
    assert!(head < 200 * 200, "{head} steps for 200 rows");
}

#[test]
fn ties_at_one_instant_page_by_id_without_duplicates_or_skips() {
    let fx = Fx::new();
    fx.events(std::iter::repeat_n(7, 1234));
    let pages = fx.all_pages(200);
    assert_eq!(pages.len(), 7);
    let got = ids(&pages);
    assert_no_duplicates(&got);
    assert_eq!(got.iter().copied().collect::<BTreeSet<_>>(), all_ids(&fx));
    assert!(got.windows(2).all(|w| w[0] > w[1]), "newest id first");
}

#[test]
fn rows_inserted_while_paging_neither_repeat_nor_hide_existing_ones() {
    let fx = Fx::new();
    fx.events(1..=300);
    let first = fx.page(None, 100);
    let cursor = first.next_cursor.unwrap();
    // Newer than the cursor, at the cursor's own instant (higher id), and a
    // late older row behind it.
    fx.events([900, cursor.ts_ms, 5]);
    let mut rest_pages = Vec::new();
    let mut next = Some(cursor);
    while let Some(c) = next {
        let p = fx.page(Some(c), 200);
        next = p.next_cursor;
        rest_pages.push(p);
    }
    let rest_events: Vec<&EventView> = rest_pages.iter().flat_map(|p| &p.events).collect();
    let seen: Vec<i64> = first
        .events
        .iter()
        .chain(rest_events.iter().copied())
        .map(|e| e.id)
        .collect();
    assert_no_duplicates(&seen);
    let ts: Vec<i64> = rest_events.iter().map(|e| e.ts_ms).collect();
    assert!(
        !ts.contains(&900),
        "ahead of the cursor: a re-read of the head shows it"
    );
    assert_eq!(
        ts.iter().filter(|t| **t == 5).count(),
        2,
        "the original ts-5 row and the late one, once each"
    );
    let existing: BTreeSet<i64> = (1..=300).collect();
    let seen_set: BTreeSet<i64> = seen.iter().copied().collect();
    assert!(existing.is_subset(&seen_set), "no existing row was skipped");
}

#[test]
fn a_reused_newest_rowid_does_not_duplicate_or_hide_anything() {
    let fx = Fx::new();
    fx.events(1..=300);
    let first = fx.page(None, 100);
    let cursor = first.next_cursor.unwrap();
    // The newest row's id is free again after it is deleted: a new row takes
    // it, now behind the cursor.
    fx.conn()
        .execute("DELETE FROM events WHERE id = 300", [])
        .unwrap();
    fx.events([150]);
    let reused: i64 = fx
        .conn()
        .query_row("SELECT max(id) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(reused, 300, "the rowid was reused");
    let rest = fx.page(Some(cursor), 1000);
    let seen: Vec<i64> = rest.events.iter().map(|e| e.id).collect();
    assert_no_duplicates(&seen);
    assert_eq!(rest.events.iter().filter(|e| e.ts_ms == 150).count(), 2);
}

#[test]
fn paging_into_evicted_data_resumes_and_ends_at_the_retention_floor() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 700;
    fx.session(1);
    fx.events(1..=600);
    let first = fx.page(None, 100);
    let cursor = first.next_cursor.unwrap();
    // Retention removes the oldest 250 rows, including none the cursor
    // points at yet (the cursor row is ts 501, still there)...
    evict_all_before(&mut fx, 251);
    let mut pages = vec![first];
    let mut next = Some(cursor);
    while let Some(c) = next {
        let p = fx.page(Some(c), 100);
        next = p.next_cursor;
        pages.push(p);
    }
    let last = pages.last().unwrap();
    assert_eq!(last.end, End::RetentionFloor, "says it ends at the floor");
    assert_eq!(last.retention_floor_ms, Some(251));
    assert!(last.gaps.iter().any(|g| g.cause == "evicted"));
    assert_no_duplicates(&ids(&pages));
    // The evicted stretch is covered by a marker, never blank.
    assert!(last
        .coverage
        .iter()
        .all(|s| s.kind != super::timeline::Kind::Unexplained));
}

#[test]
fn a_cursor_whose_row_was_evicted_still_resumes_at_the_next_older_row() {
    let mut fx = Fx::new();
    fx.session(1);
    fx.events(1..=600);
    let first = fx.page(None, 200);
    let cursor = first.next_cursor.unwrap();
    assert_eq!(cursor.ts_ms, 401);
    evict_all_before(&mut fx, 450);
    // The row the cursor names is gone, and so is everything older.
    let rest = fx.page(Some(cursor), 100);
    assert!(rest.events.is_empty(), "nothing older survives");
    assert_eq!(rest.end, End::RetentionFloor);
    assert!(rest.next_cursor.is_none());
    assert!(
        rest.gaps.iter().any(|g| g.cause == "evicted"),
        "and it says so"
    );
}

/// Retention removes in chunks: runs them until nothing older than `cutoff`
/// is left.
fn evict_all_before(fx: &mut Fx, cutoff: i64) {
    loop {
        let older: i64 = fx
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE ts_ms < ?1",
                [cutoff],
                |r| r.get(0),
            )
            .unwrap();
        if older == 0 {
            return;
        }
        evict_before(&mut fx.rig, cutoff).unwrap();
    }
}

fn consistent_store(fx: &Fx) {
    fx.session(1_000);
    fx.events((1_000..=1_400).step_by(2));
    fx.end_session(1_450, SessionEnd::Clean);
    fx.gap(1_450, 1_500, GapCause::BridgeDown, 1);
    fx.session(1_500);
    fx.events((1_500..=1_900).step_by(2));
    fx.gap(1_700, 1_720, GapCause::UpstreamLoss, 4);
    fx.gap(1_800, 1_805, GapCause::QueueOverflow, 2);
}

#[test]
fn consecutive_pages_share_a_boundary_and_each_tiles_its_window() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1_500);
    fx.now_ms = 2_000;
    consistent_store(&fx);
    let pages = fx.all_pages(37);
    assert!(pages.len() > 5);
    for page in &pages {
        let c = &page.coverage;
        assert_eq!(c.first().unwrap().start_ms, page.window_lo_ms);
        assert_eq!(c.last().unwrap().end_ms, page.window_hi_ms);
        assert!(
            c.windows(2).all(|w| w[0].end_ms == w[1].start_ms),
            "a hole inside a page"
        );
        assert!(
            c.iter()
                .all(|s| s.kind != super::timeline::Kind::Unexplained),
            "{c:?}"
        );
    }
    for pair in pages.windows(2) {
        assert_eq!(
            pair[0].window_lo_ms, pair[1].window_hi_ms,
            "a hole between pages"
        );
    }
    assert_eq!(pages[0].window_hi_ms, 2_000, "the head runs to now");
    assert_no_duplicates(&ids(&pages));
    assert_eq!(ids(&pages).len(), all_ids(&fx).len());
    // Every gap row is on some page that overlaps it.
    for stored in [(1_450, 1_500), (1_700, 1_720), (1_800, 1_805)] {
        assert!(
            pages
                .iter()
                .any(|p| p.gaps.iter().any(|g| (g.start_ms, g.end_ms) == stored)),
            "gap {stored:?} never shown"
        );
    }
    // Every non-final page moves the cursor strictly forward.
    let keys: Vec<(i64, i64)> = pages
        .iter()
        .filter_map(|p| p.next_cursor)
        .map(|c| (c.ts_ms, c.id))
        .collect();
    assert!(keys.windows(2).all(|w| w[0] > w[1]), "{keys:?}");
}

#[test]
fn eviction_between_pages_leaves_no_hole_between_windows() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1_500);
    fx.now_ms = 2_000;
    consistent_store(&fx);
    let mut pages = vec![fx.page(None, 40)];
    evict_all_before(&mut fx, 1_300);
    while let Some(c) = pages.last().unwrap().next_cursor {
        pages.push(fx.page(Some(c), 40));
    }
    for pair in pages.windows(2) {
        assert_eq!(pair[0].window_lo_ms, pair[1].window_hi_ms);
    }
    for page in &pages {
        assert!(
            page.coverage
                .iter()
                .all(|s| s.kind != super::timeline::Kind::Unexplained),
            "{:?}",
            page.coverage
        );
    }
    assert_eq!(pages.last().unwrap().end, End::RetentionFloor);
}
