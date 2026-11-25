//! Limits: rows, bytes, time and scan are bounded by the reader itself, and
//! no read lock outlives a statement.

use std::collections::BTreeSet;
use std::time::Duration;

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_size::event_bytes;

const KIB: usize = 1024;

/// A string of `n` control characters, as SQL: the worst case for JSON, six
/// bytes each.
fn ctl(n: usize) -> String {
    format!("replace(hex(zeroblob({n})), '00', char(1))")
}

fn worst_case_events(fx: &Fx, n: i64) {
    let sql = format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {n}) \
         INSERT INTO events (ts_ms, process_path, dst_host, dst_ip, protocol, action, \
             answered_by, rule_name, rule_class, src_ip, dst_port, src_port, decided_ts_ms) \
         SELECT x, {path}, {host}, {ip}, {proto}, 'allow', 'rule', {name}, {class}, {ip}, \
             65535, 65535, 9223372036854775807 FROM c",
        path = ctl(4096),
        host = ctl(253),
        ip = ctl(39),
        proto = ctl(16),
        name = ctl(256),
        class = ctl(24),
    );
    fx.conn().execute_batch(&sql).unwrap();
}

fn all_event_ids(pages: &[Page]) -> Vec<i64> {
    pages
        .iter()
        .flat_map(|p| p.events.iter().map(|e| e.id))
        .collect()
}

fn assert_pages_complete(pages: &[Page], expected: i64) {
    let ids = all_event_ids(pages);
    let unique: BTreeSet<_> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "a duplicate");
    assert_eq!(unique, (1..=expected).collect::<BTreeSet<_>>(), "a skip");
}

#[test]
fn the_defaults_are_the_designs_bounds() {
    let l = Limits::default();
    assert_eq!(l.events_per_page, 200);
    assert_eq!(l.gap_rows_per_page, 100);
    assert_eq!(l.session_rows_per_page, 100);
    assert_eq!(l.page_bytes, 256 * KIB);
    assert_eq!(l.query_deadline, Duration::from_secs(2));
    // A statement must end before the writer gives up on its commit (250 ms).
    assert!(l.statement_budget <= Duration::from_millis(100));
    assert!(l.busy_wait + l.statement_budget < Duration::from_millis(250));
}

#[test]
fn worst_case_rows_make_a_page_under_the_byte_bound_with_a_cursor() {
    let fx = Fx::new();
    worst_case_events(&fx, 300);
    let pages = fx.all_pages(200);
    assert!(pages.len() > 10, "{} pages", pages.len());
    for page in &pages {
        assert!(page.bytes <= 256 * KIB, "{}", page.bytes);
        let json = serde_json::to_vec(page).unwrap();
        assert!(
            json.len() <= page.bytes,
            "real {} > bound {}",
            json.len(),
            page.bytes
        );
        for e in &page.events {
            let real = serde_json::to_string(e).unwrap().len();
            assert!(real <= event_bytes(e), "row {real} > {}", event_bytes(e));
        }
    }
    assert!(pages[0].partial == Some(Partial::Bytes));
    assert!(pages[0].next_cursor.is_some());
    assert_pages_complete(&pages, 300);
}

#[test]
fn a_page_of_ordinary_rows_is_exactly_the_row_limit_and_under_the_bound() {
    let fx = Fx::new();
    fx.events(1..=201);
    let page = fx.page(None, 200);
    assert_eq!(page.events.len(), 200);
    assert!(page.partial.is_none());
    assert!(page.next_cursor.is_some());
    assert!(page.bytes <= 256 * KIB);
    assert!(serde_json::to_vec(&page).unwrap().len() <= page.bytes);
}

#[test]
fn gap_heavy_windows_shrink_the_window_and_drop_no_gap() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1_000);
    fx.now_ms = 1_300;
    fx.session(1_000);
    fx.events(1_000..1_200);
    let causes = [
        GapCause::UpstreamLoss,
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
    ];
    for i in 0..150i64 {
        fx.gap(1_000 + i, 1_000 + i, causes[(i % 3) as usize], 1);
    }
    let first = fx.page(None, 200);
    assert_eq!(first.partial, Some(Partial::Gaps));
    assert!(first.gaps.len() <= 100, "{}", first.gaps.len());
    assert!(first.events.len() < 200);
    assert!(first.bytes <= 256 * KIB);
    assert!(serde_json::to_vec(&first).unwrap().len() <= first.bytes);
    let pages = fx.all_pages(200);
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    for page in &pages {
        assert!(page.gaps.len() <= 100);
        assert!(page
            .coverage
            .iter()
            .all(|s| s.kind != super::timeline::Kind::Unexplained || s.start_ms > 1_200));
        seen.extend(page.gaps.iter().map(|g| g.start_ms));
    }
    assert_eq!(
        seen,
        (1_000..1_150).collect::<BTreeSet<_>>(),
        "every gap row shown"
    );
    let keys: Vec<(i64, i64)> = pages
        .iter()
        .filter_map(|p| p.next_cursor)
        .map(|c| (c.ts_ms, c.id))
        .collect();
    assert!(
        keys.windows(2).all(|w| w[0] > w[1]),
        "strictly forward: {keys:?}"
    );
}

#[test]
fn more_gap_rows_at_one_instant_than_a_page_holds_is_said_not_hidden() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1_000);
    fx.now_ms = 1_100;
    fx.session(1_000);
    fx.events([1_050]);
    let causes = [
        GapCause::UpstreamLoss,
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
    ];
    for i in 0..150usize {
        fx.gap(1_050, 1_050, causes[i % 3], 1);
    }
    let page = fx.page(None, 200);
    assert_eq!(page.partial, Some(Partial::Gaps));
    assert!(page.gaps.len() <= 100);
    assert!(page.bytes <= 256 * KIB);
}

#[test]
fn a_zero_query_deadline_returns_a_partial_page_that_resumes_cleanly() {
    let mut fx = Fx::new();
    fx.events(1..=100);
    fx.limits.query_deadline = Duration::ZERO;
    fx.limits.scan_window = 10;
    let pages = fx.all_pages(200);
    // Ten pages cut at the deadline, then the empty page that finds the end.
    assert_eq!(pages.len(), 11);
    assert!(pages[..10].iter().all(|p| p.partial == Some(Partial::Time)));
    assert!(pages[..10].iter().all(|p| p.events.len() == 10));
    assert!(pages[10].events.is_empty());
    assert_eq!(pages[10].end, End::OldestRecorded);
    assert_pages_complete(&pages, 100);
}

#[test]
fn a_statement_over_its_budget_is_cut_but_keeps_its_rows_and_resumes() {
    let mut fx = Fx::new();
    fx.events(1..=300);
    fx.limits.statement_budget = Duration::ZERO;
    let mut pages = Vec::new();
    let mut cursor = None;
    loop {
        let page = fx.page(cursor, 200);
        assert!(
            !page.events.is_empty(),
            "a cut statement still made progress"
        );
        cursor = page.next_cursor;
        pages.push(page);
        assert!(pages.len() < 400);
        if cursor.is_none() {
            break;
        }
    }
    assert!(
        pages.iter().any(|p| p.partial == Some(Partial::Time)),
        "the budget cut something"
    );
    assert_pages_complete(&pages, 300);
}

#[test]
fn the_scan_cap_ends_a_page_with_a_cursor_that_advances() {
    let mut fx = Fx::new();
    fx.events(1..=300);
    fx.limits.scan_rows_max = 50;
    fx.limits.scan_window = 20;
    let pages = fx.all_pages(200);
    assert_eq!(pages[0].events.len(), 50);
    assert_eq!(pages[0].partial, Some(Partial::Scan));
    assert_pages_complete(&pages, 300);
}

#[test]
fn no_read_lock_is_held_between_statements() {
    let fx = Fx::new();
    fx.events(1..=300);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 100,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    // The reader's connection is open and idle: a writer commits at once,
    // with no busy wait at all.
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    writer
        .execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .expect("no lock held between stages");
    read.load_ledger().unwrap_or_else(|o| panic!("{o:?}"));
    writer
        .execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .expect("none after the ledger either");
    assert!(matches!(read.finish(), Outcome::Page(_)));
}

fn accounted(page: &Page) -> usize {
    use super::reader_size::{gap_bytes, segment_bytes, session_bytes, HEADER_BYTES};
    HEADER_BYTES
        + page.events.iter().map(event_bytes).sum::<usize>()
        + page.gaps.iter().map(gap_bytes).sum::<usize>()
        + page.sessions.iter().map(session_bytes).sum::<usize>()
        + page.coverage.iter().map(segment_bytes).sum::<usize>()
}

#[test]
fn the_byte_total_counts_gaps_sessions_and_segments_as_well_as_events() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 50_000;
    fx.session(1);
    fx.events(1..=150);
    for i in 0..60i64 {
        fx.gap(10 + i, 40_000, GapCause::ClockJump, 1);
        fx.gap(5_000 + i, 5_000 + i, GapCause::UpstreamLoss, 2);
    }
    let page = fx.page(None, 200);
    assert!(!page.gaps.is_empty() && !page.sessions.is_empty() && !page.coverage.is_empty());
    assert_eq!(
        page.bytes,
        accounted(&page),
        "every part of the page is counted"
    );
}

#[test]
fn overlapping_gaps_beside_worst_case_events_still_fit_the_bound() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 50_000;
    fx.session(1);
    worst_case_events(&fx, 150);
    // Many overlapping gap rows (rotating causes, so they do not merge):
    // every segment lists the gaps that cover it.
    let causes = [
        GapCause::UpstreamLoss,
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
    ];
    // A smaller page, so the ledger's bytes are what tips it over.
    fx.limits.page_bytes = 245 * KIB;
    for i in 0..94usize {
        fx.gap(i as i64, 40_000, causes[i % 3], 1);
    }
    let pages = fx.all_pages(200);
    assert!(
        pages[0].events.len() < 8,
        "the ledger's bytes cost events: {}",
        pages[0].events.len()
    );
    for page in pages {
        assert!(page.bytes <= 245 * KIB, "{}", page.bytes);
        assert_eq!(page.bytes, accounted(&page));
        assert!(serde_json::to_vec(&page).unwrap().len() <= page.bytes);
    }
}
