//! Repair round on the reader: history off, public limit values, and the
//! bound on the duplicate lookup.

use std::time::{Duration, Instant};

use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_sql::DUPLICATE_PROBE_CAP;

fn unavailable(outcome: Outcome) -> Unavail {
    match outcome {
        Outcome::Unavailable(u) => u,
        other => panic!("expected unavailable, got {other:?}"),
    }
}

// ---- history off ----

fn switch_off(fx: &Fx) {
    let mut settings = super::settings::load(&fx.rig.location).unwrap().unwrap();
    settings.enabled = false;
    super::settings::save(&fx.rig.location, &settings).unwrap();
}

#[test]
fn a_retained_database_is_not_served_while_history_is_off() {
    let fx = Fx::new();
    fx.events(1..=5);
    switch_off(&fx);
    assert!(fx.db.exists(), "the database is retained");
    let u = unavailable(fx.read(None, 10));
    assert_eq!(u.kind, UnavailKind::Off, "{}", u.reason);
    assert!(!u.retry);
}

#[test]
fn history_off_is_answered_without_opening_the_file() {
    let fx = Fx::new();
    fx.events(1..=5);
    switch_off(&fx);
    // Opening would say "no history" (NotFound); off is answered first.
    std::fs::remove_file(&fx.db).unwrap();
    let u = unavailable(fx.read(None, 10));
    assert_eq!(u.kind, UnavailKind::Off, "{}", u.reason);
}

// ---- public limit values ----

#[test]
fn zero_events_per_page_is_one_event_not_a_panic() {
    let mut fx = Fx::new();
    fx.events(1..=5);
    fx.limits.events_per_page = 0;
    let page = fx.page(None, 10);
    assert_eq!(page.events.len(), 1);
    assert!(page.next_cursor.is_some());
}

#[test]
fn a_page_byte_limit_below_a_page_is_refused_not_wrapped() {
    for bytes in [0, 1, 100, 4095, 4096] {
        let mut fx = Fx::new();
        fx.events(1..=5);
        fx.limits.page_bytes = bytes;
        let u = unavailable(fx.read(None, 10));
        assert_eq!(u.kind, UnavailKind::LimitsTooSmall, "{bytes}: {}", u.reason);
        assert!(!u.retry);
    }
}

#[test]
fn the_largest_limits_saturate_instead_of_overflowing() {
    // The ceilings, not the caller's numbers, bound the page: 300 events are
    // two pages however large the limits are asked to be.
    let mut fx = Fx::new();
    fx.events(1..=300);
    fx.limits.events_per_page = usize::MAX;
    let page = fx.page(None, usize::MAX);
    assert_eq!(page.events.len(), 200);
    fx.limits.scan_rows_max = usize::MAX;
    fx.limits.scan_window = usize::MAX;
    let pages = fx.all_pages(usize::MAX);
    let total: usize = pages.iter().map(|p| p.events.len()).sum();
    assert_eq!(total, 300);
}

#[test]
fn a_zero_scan_window_still_moves_the_page_forward() {
    let mut fx = Fx::new();
    fx.events(1..=5);
    fx.limits.scan_window = 0;
    let page = fx.page(None, 10);
    assert_eq!(page.events.len(), 5);
}

// ---- the duplicate lookup is bounded ----

/// Prompts and pings that never match each other (different processes), all
/// at one instant: every lookup has `prompts` or `pings` candidates.
fn dense_store(fx: &Fx, prompts: u32, pings: u32) {
    let sql = format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {prompts}) \
         INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
         SELECT 1000, 1000, 'replied', '/a', 'allow', 'gui' FROM c; \
         WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {pings}) \
         INSERT INTO events (ts_ms, process_path, action, answered_by) \
         SELECT 1000, '/b', 'allow', 'rule' FROM c;"
    );
    fx.conn().execute_batch(&sql).unwrap();
}

#[test]
fn a_default_request_over_sixty_thousand_prompts_finishes_within_the_budget() {
    let fx = Fx::new();
    dense_store(&fx, 60_000, 300);
    let started = Instant::now();
    let page = fx.page(None, 200);
    let took = started.elapsed();
    assert!(took < fx.limits.query_deadline, "{took:?}");
    // 200 rows at the fixed cap may run past one statement budget on a slow
    // machine: then the page is cut with the rows read and a cursor.
    assert!(!page.events.is_empty(), "progress");
    match page.partial {
        None => assert_eq!(page.events.len(), 200),
        Some(why) => {
            assert_eq!(why, Partial::Time);
            assert!(page.next_cursor.is_some());
        }
    }
}

#[test]
fn repeated_requests_over_the_dense_store_each_make_progress() {
    let fx = Fx::new();
    dense_store(&fx, 60_000, 300);
    let mut cursor = None;
    for _ in 0..5 {
        let page = fx.page(cursor, 200);
        assert!(!page.events.is_empty(), "a retry that moves nothing");
        assert_ne!(page.next_cursor, cursor);
        cursor = page.next_cursor;
    }
}

/// A zero statement budget cuts the first statement before any row: the
/// only way to a cut that read no event, deterministically.
fn first_outcome_under_a_cut(limit: usize) -> Outcome {
    let mut fx = Fx::new();
    dense_store(&fx, 60_000, 300);
    fx.limits.statement_budget = Duration::ZERO;
    fx.read(None, limit)
}

#[test]
fn a_time_cut_with_no_event_at_a_page_larger_than_one_says_ask_for_less() {
    // The same request would be cut again: retry:true would loop forever.
    let u = unavailable(first_outcome_under_a_cut(200));
    assert_eq!(u.kind, UnavailKind::TooExpensive, "{}", u.reason);
    assert!(!u.retry, "{}", u.reason);
}

#[test]
fn a_time_cut_with_no_event_at_a_page_of_one_is_retryable() {
    // Nothing smaller exists to ask for.
    let u = unavailable(first_outcome_under_a_cut(1));
    assert_ne!(u.kind, UnavailKind::TooExpensive, "{}", u.reason);
    assert!(u.retry, "{}", u.reason);
}

fn pings_with_prompts(fx: &Fx, prompts: usize) {
    let sql = format!(
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < {prompts}) \
         INSERT INTO events (ts_ms, decided_ts_ms, prompt_outcome, process_path, action, answered_by) \
         SELECT 1000, 1000, 'replied', '/a', 'allow', 'gui' FROM c; \
         INSERT INTO events (ts_ms, process_path, action, answered_by) \
         VALUES (1000, '/b', 'allow', 'rule');"
    );
    fx.conn().execute_batch(&sql).unwrap();
}

#[test]
fn the_flag_is_exact_up_to_the_cap_and_undetermined_past_it() {
    // Never matching: the exact answer is false.
    let at_cap = Fx::new();
    pings_with_prompts(&at_cap, DUPLICATE_PROBE_CAP);
    let ping = at_cap.page(None, 1).events.remove(0);
    assert!(!ping.possible_duplicate, "exact within the cap");
    let past = Fx::new();
    pings_with_prompts(&past, DUPLICATE_PROBE_CAP + 1);
    let ping = past.page(None, 1).events.remove(0);
    assert!(ping.possible_duplicate, "cannot be ruled out past the cap");
}

#[test]
fn a_rows_duplicate_flag_does_not_depend_on_the_page_size() {
    // 30 prompts within two seconds of every ping, well under the cap. A cap
    // shared out over the page (here 600 / 201 = 2 at 200 rows) would flag it
    // at 200 rows but not at 1.
    let mut fx = Fx::new();
    dense_store(&fx, 30, 200);
    fx.limits.scan_rows_max = 600;
    let flags: Vec<bool> = [1, 50, 200]
        .iter()
        .map(|&limit| fx.page(None, limit).events[0].possible_duplicate)
        .collect();
    assert_eq!(flags, vec![false, false, false], "one answer per row");
}
