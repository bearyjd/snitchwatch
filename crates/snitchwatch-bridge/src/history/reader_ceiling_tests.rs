//! Hard ceilings inside the reader: a public [`Limits`] value can lower a
//! bound but never raise one past what the writer's 250 ms lock wait and the
//! memory budget allow.

use std::time::Duration;

use super::reader::*;
use super::reader_fixture::Fx;

#[test]
fn a_maximum_busy_wait_is_clamped_not_a_panic() {
    let mut fx = Fx::new();
    fx.events(1..=5);
    fx.limits.busy_wait = Duration::MAX;
    let page = fx.page(None, 10);
    assert_eq!(page.events.len(), 5);
}

fn everything_raised() -> Limits {
    Limits {
        events_per_page: usize::MAX,
        gap_rows_per_page: usize::MAX,
        session_rows_per_page: usize::MAX,
        page_bytes: usize::MAX,
        statement_budget: Duration::from_secs(600),
        query_deadline: Duration::from_secs(600),
        scan_window: usize::MAX,
        scan_rows_max: usize::MAX,
        busy_wait: Duration::MAX,
    }
}

#[test]
fn raised_limits_are_clamped_to_the_hard_ceilings() {
    let ceiling = Limits::default();
    let bounds = everything_raised().clamped();
    assert_eq!(bounds.events_per_page, ceiling.events_per_page);
    assert_eq!(bounds.gap_rows_per_page, ceiling.gap_rows_per_page);
    assert_eq!(bounds.session_rows_per_page, ceiling.session_rows_per_page);
    assert_eq!(bounds.page_bytes, ceiling.page_bytes);
    assert_eq!(bounds.statement_budget, ceiling.statement_budget);
    assert_eq!(bounds.query_deadline, ceiling.query_deadline);
    assert_eq!(bounds.scan_window, ceiling.scan_window);
    assert_eq!(bounds.scan_rows_max, ceiling.scan_rows_max);
    assert!(
        bounds.busy_wait < Duration::from_millis(250),
        "{:?}",
        bounds.busy_wait
    );
    // Lowering still works.
    let low = Limits {
        statement_budget: Duration::from_millis(5),
        ..Limits::default()
    };
    assert_eq!(low.clamped().statement_budget, Duration::from_millis(5));
}

#[test]
fn a_caller_cannot_raise_the_page_past_the_row_and_coverage_ceilings() {
    let mut fx = Fx::new();
    fx.events(1..=300);
    // Nested gaps: every one overlaps every other, so each coverage segment
    // lists all of them.
    for i in 0..300i64 {
        fx.gap(10 + i, 100_000 - i, super::ledger::GapCause::WriteFailed, 1);
    }
    fx.limits = everything_raised();
    let page = fx.page(None, usize::MAX);
    let ceiling = Limits::default();
    assert!(
        page.events.len() <= ceiling.events_per_page,
        "{}",
        page.events.len()
    );
    assert!(
        page.gaps.len() <= ceiling.gap_rows_per_page,
        "{}",
        page.gaps.len()
    );
    assert!(page.bytes <= ceiling.page_bytes, "{}", page.bytes);
    let indexes: usize = page.coverage.iter().map(|s| s.gaps.len()).sum();
    assert!(
        indexes <= 100 * (2 * 100 + 17),
        "{indexes} coverage indexes"
    );
}
