//! A page that cannot carry all of its ledger says so: what it leaves out is
//! unexplained (never recorded), it stays inside its bounds, and building it
//! costs a bounded amount whatever the ledger holds.

use super::ledger::GapCause;
use super::reader::*;
use super::reader_fixture::Fx;
use super::timeline::{coalesce, coverage_masked, Kind, Window};

const KIB: usize = 1024;

fn ctl(n: usize) -> String {
    format!("replace(hex(zeroblob({n})), '00', char(1))")
}

fn tiles(page: &Page) {
    let first = page.coverage.first().expect("a segment");
    let last = page.coverage.last().unwrap();
    assert_eq!(
        (first.start_ms, last.end_ms),
        (page.window_lo_ms, page.window_hi_ms)
    );
    for pair in page.coverage.windows(2) {
        assert_eq!(pair[0].end_ms, pair[1].start_ms, "a hole: {pair:?}");
    }
}

fn within_bounds(page: &Page, limits: &Limits) {
    assert!(page.bytes <= limits.page_bytes, "{} bytes", page.bytes);
    let json = serde_json::to_vec(page).unwrap();
    assert!(
        json.len() <= page.bytes,
        "real {} > {}",
        json.len(),
        page.bytes
    );
}

// ---- a dropped gap is never recorded ----

#[test]
fn a_gap_the_page_could_not_carry_is_never_labelled_recorded() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 2_000;
    fx.session(1);
    fx.events([1_000]);
    let causes = [
        GapCause::UpstreamLoss,
        GapCause::QueueOverflow,
        GapCause::AdmissionDrop,
    ];
    // 101 point gaps between the event and the head: one more than a page
    // carries, and the oldest (nearest the event) is the one left out.
    for i in 0..101 {
        fx.gap(1_001 + i, 1_001 + i, causes[i as usize % 3], 1);
    }
    let page = fx.page(None, 200);
    assert_eq!(page.partial, Some(Partial::Gaps));
    assert_eq!(page.gaps.len(), 100);
    tiles(&page);
    for t in 1_001..=1_101 {
        let point = page
            .coverage
            .iter()
            .find(|s| s.start_ms == t && s.end_ms == t)
            .unwrap_or_else(|| panic!("no segment of its own at {t}"));
        assert_ne!(point.kind, Kind::Recorded, "{t}: {point:?}");
    }
}

#[test]
fn masked_coverage_cuts_at_dropped_edges_and_points() {
    let w = Window::new(0, 100).unwrap();
    let session = super::session::Session {
        start_ms: 0,
        end_ms: None,
        end: None,
    };
    let segments = coverage_masked(w, &[session], &[], None, &[(10, 10), (40, 60)]);
    let kinds: Vec<_> = segments
        .iter()
        .map(|s| (s.start_ms, s.end_ms, s.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            (0, 10, Kind::Recorded),
            (10, 10, Kind::Unexplained),
            (10, 40, Kind::Recorded),
            (40, 60, Kind::Unexplained),
            (60, 100, Kind::Recorded),
        ]
    );
}

#[test]
fn dropped_spans_merge_and_collapse_past_the_bound() {
    assert_eq!(coalesce([(5, 9), (1, 6), (20, 20)], 4), [(1, 9), (20, 20)]);
    let many: Vec<_> = (0..40).map(|i| (i * 10, i * 10 + 1)).collect();
    assert_eq!(coalesce(many, 16), [(0, 391)]);
}

// ---- the byte bound holds on the trimmed path too ----

fn nested_ledger(fx: &Fx, gaps: u32, sessions: u32, span: i64) {
    let sql = format!(
        "WITH RECURSIVE c(x) AS (SELECT 0 UNION ALL SELECT x + 1 FROM c WHERE x < {top}) \
         INSERT INTO gaps (start_ms, end_ms, start_exact, cause, count) \
         SELECT x, {span} - x, 1, '{cause}', 1 FROM c WHERE x < {gaps}; \
         WITH RECURSIVE c(x) AS (SELECT 0 UNION ALL SELECT x + 1 FROM c WHERE x < {top}) \
         INSERT INTO sessions (start_ms, end_ms, end_kind) \
         SELECT {gaps} + 5 + x * 10, {gaps} + 5 + x * 10 + 5, 'clean' FROM c WHERE x < {sessions};",
        top = gaps.max(sessions),
        cause = GapCause::UpstreamLoss.as_str(),
    );
    fx.conn().execute_batch(&sql).unwrap();
}

#[test]
fn a_hundred_overlapping_gaps_over_a_hundred_sessions_stay_under_the_default_cap() {
    let mut fx = Fx::new();
    fx.now_ms = 100_000;
    fx.events([50_000]);
    nested_ledger(&fx, 100, 100, 90_000);
    let page = fx.page(None, 200);
    within_bounds(&page, &fx.limits);
    assert_eq!(page.partial, Some(Partial::Gaps));
    tiles(&page);
}

#[test]
fn a_page_limit_too_small_for_the_rows_trims_them_and_still_obeys_it() {
    let mut fx = Fx::new();
    fx.now_ms = 100_000;
    fx.events([50_000]);
    nested_ledger(&fx, 100, 100, 90_000);
    fx.limits.page_bytes = 12 * KIB;
    let page = fx.page(None, 200);
    within_bounds(&page, &fx.limits);
    assert!(page.gaps.len() < 100, "rows were shed: {}", page.gaps.len());
    tiles(&page);
}

#[test]
fn an_event_larger_than_the_page_limit_is_refused_not_delivered_oversized() {
    let mut fx = Fx::new();
    fx.conn()
        .execute_batch(&format!(
            "INSERT INTO events (ts_ms, process_path, action, answered_by) \
             VALUES (10, {}, 'allow', 'rule')",
            ctl(4096)
        ))
        .unwrap();
    // The event is about 24 KiB escaped; the limit leaves it 4 KiB.
    fx.limits.page_bytes = 8 * KIB;
    match fx.read(None, 10) {
        Outcome::Unavailable(u) => {
            assert_eq!(u.kind, UnavailKind::OversizedEvent, "{}", u.reason);
            assert!(!u.retry);
        }
        other => panic!("{other:?}"),
    }
    // Under the default cap the same event is an ordinary page.
    fx.limits = Limits::default();
    let page = fx.page(None, 10);
    assert_eq!(page.events.len(), 1);
    within_bounds(&page, &fx.limits);
}

// ---- the work is bounded by the row caps, not by the ledger ----

#[test]
fn four_thousand_nested_gaps_over_four_thousand_sessions_are_cut_to_a_small_page() {
    let mut fx = Fx::new();
    fx.now_ms = 100_000;
    fx.events([50_000]);
    nested_ledger(&fx, 4_096, 4_096, 100_000);
    let started = std::time::Instant::now();
    let page = fx.page(None, 200);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    within_bounds(&page, &fx.limits);
    assert_eq!(page.partial, Some(Partial::Gaps));
    // Rows and segments are what the page carries, not what the ledger holds.
    assert!(page.gaps.len() <= 100 && page.sessions.len() <= 100);
    assert!(
        page.coverage.len() <= 1_000,
        "{} segments",
        page.coverage.len()
    );
    tiles(&page);
}
