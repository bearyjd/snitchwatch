//! The reader's stages: a cancel, a generation change or an expired deadline
//! landing between two of them (or during the last one) is never lost, and
//! never delivered as if nothing happened.

use std::time::Duration;

use super::reader::*;
use super::reader_fixture::Fx;
use super::timeline::Kind;

fn staged<'a>(fx: &'a Fx, ctx: &'a Context<'a>, limit: usize) -> Read<'a> {
    let _ = fx;
    let request = Request {
        cursor: None,
        limit,
    };
    let mut read = Read::open(&request, ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    read.load_ledger().unwrap_or_else(|o| panic!("{o:?}"));
    read
}

#[test]
fn a_cancel_that_lands_while_the_page_is_assembled_delivers_nothing() {
    let fx = Fx::new();
    fx.events(1..=20);
    let ctx = fx.ctx();
    let built = staged(&fx, &ctx, 10)
        .build()
        .unwrap_or_else(|o| panic!("{o:?}"));
    fx.cancel.cancel();
    assert!(matches!(built.deliver(), Outcome::Cancelled));
}

#[test]
fn a_generation_change_while_the_page_is_assembled_delivers_nothing() {
    let fx = Fx::new();
    fx.events(1..=20);
    let ctx = fx.ctx();
    let built = staged(&fx, &ctx, 10)
        .build()
        .unwrap_or_else(|o| panic!("{o:?}"));
    fx.generation.advance().unwrap();
    assert!(matches!(built.deliver(), Outcome::Superseded));
}

#[test]
fn an_undisturbed_build_is_delivered() {
    let fx = Fx::new();
    fx.events(1..=20);
    let ctx = fx.ctx();
    let built = staged(&fx, &ctx, 10)
        .build()
        .unwrap_or_else(|o| panic!("{o:?}"));
    assert!(matches!(built.deliver(), Outcome::Page(_)));
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

#[test]
fn an_expired_ledger_stage_keeps_the_collected_events_and_their_cursor() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.session(1);
    fx.events(1..=40);
    fx.limits.query_deadline = Duration::from_millis(200);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 10,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    std::thread::sleep(Duration::from_millis(250));
    read.load_ledger()
        .unwrap_or_else(|o| panic!("the rows were lost: {o:?}"));
    let Outcome::Page(page) = read.finish() else {
        panic!("a page")
    };
    assert_eq!(page.events.len(), 10, "every collected event");
    assert_eq!(page.partial, Some(Partial::Time));
    assert_eq!(page.end, End::More);
    assert_eq!(page.next_cursor.map(|c| c.ts_ms), Some(31));
    tiles(&page);
    // What the ledger would have said is unknown: never "recorded".
    assert!(
        page.coverage.iter().all(|s| s.kind != Kind::Recorded),
        "{:?}",
        page.coverage
    );
}

#[test]
fn an_expired_deadline_with_no_events_collected_is_unavailable_not_empty() {
    let mut fx = Fx::new();
    fx.limits.query_deadline = Duration::from_millis(50);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 10,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    std::thread::sleep(Duration::from_millis(100));
    let ledger = read.load_ledger();
    assert!(
        matches!(ledger, Err(Outcome::Unavailable(ref u)) if u.retry),
        "{ledger:?}"
    );
}
