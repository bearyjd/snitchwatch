//! What a page says about the stretch it covers: outages written by the real
//! writer path, evicted markers, a crashed session, standing labels, and
//! counts that are lower bounds.

use std::sync::Mutex;

use super::caps::ProcessPath;
use super::durable::tests::{rig as durable_rig, Rig as DurableRig};
use super::gaps::{flush_due, Losses, StandingLabels};
use super::ledger::GapCause;
use super::lifecycle::Generation;
use super::reader::*;
use super::reader_fixture::Fx;
use super::reader_registry::CancelToken;
use super::record::{insert_event, Action, AnsweredBy, EventRecord};
use super::retention_support::evict_process;
use super::timeline::Kind;
use crate::cache::rule_hits::UpstreamLoss;

fn ping(missing: u64, since: Option<i64>, at: i64) -> UpstreamLoss {
    UpstreamLoss {
        missing,
        restarted: false,
        since_ms: since,
        at_ms: Some(at),
    }
}

/// An outage written by the real loss tracker: the daemon down at 2000, back
/// at 4000, and the first ping after it reporting `missing` lost upstream.
fn outage(missing: u64) -> DurableRig {
    let mut rig = durable_rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    let losses = Mutex::new(Losses::new());
    {
        let mut l = losses.lock().unwrap();
        let first = l.begin_ping(&rig.registry, ping(0, None, 1000), 0);
        l.finish_ping(&rig.registry, first);
    }
    rig.set(2000);
    losses.lock().unwrap().daemon_down(&rig.registry);
    rig.set(4000);
    losses.lock().unwrap().daemon_up(4000);
    rig.set(4500);
    losses
        .lock()
        .unwrap()
        .upstream_loss(&rig.registry, ping(missing, Some(1000), 4500));
    rig.set(5000);
    flush_due(&mut rig.engine, &rig.registry, &rig.limits, &losses, false).unwrap();
    rig
}

fn read_durable(rig: &DurableRig, live: Option<i64>, now_ms: i64) -> Page {
    let generation = Generation::default();
    let cancel = CancelToken::default();
    let ctx = Context {
        location: &rig.location,
        generation: &generation,
        cancel: &cancel,
        now_ms,
        live_session_start_ms: live,
        standing: None,
        limits: Limits::default(),
    };
    match read_page(
        &Request {
            cursor: None,
            limit: 200,
        },
        &ctx,
    ) {
        Outcome::Page(page) => *page,
        other => panic!("{other:?}"),
    }
}

fn assert_tiled_and_explained(page: &Page) {
    let c = &page.coverage;
    assert_eq!(c.first().unwrap().start_ms, page.window_lo_ms);
    assert_eq!(c.last().unwrap().end_ms, page.window_hi_ms);
    assert!(c.windows(2).all(|w| w[0].end_ms == w[1].start_ms), "{c:?}");
    assert!(c.iter().all(|s| s.kind != Kind::Unexplained), "{c:?}");
}

#[test]
fn after_an_outage_the_page_covers_last_ping_to_down_edge_outage_and_up_to_ping() {
    let rig = outage(3);
    let page = read_durable(&rig, Some(1000), 6000);
    assert_tiled_and_explained(&page);
    let causes: Vec<&str> = page.gaps.iter().map(|g| g.cause).collect();
    assert!(causes.contains(&"daemon-disconnected"), "{causes:?}");
    assert_eq!(causes.iter().filter(|c| **c == "upstream-loss").count(), 2);
    // Every instant from the session's start to now is on a segment.
    assert_eq!((page.window_lo_ms, page.window_hi_ms), (1000, 6000));
}

#[test]
fn a_single_lost_event_across_an_outage_is_at_least_one_not_two() {
    let rig = outage(1);
    let page = read_durable(&rig, Some(1000), 6000);
    let upstream: Vec<&GapView> = page
        .gaps
        .iter()
        .filter(|g| g.cause == "upstream-loss")
        .collect();
    assert_eq!(
        upstream.iter().map(|g| g.count_min).sum::<u64>(),
        2,
        "the rows say 1 + 1"
    );
    assert!(
        upstream.iter().all(|g| g.part_of_pair),
        "shown as a pair, not a sum"
    );
    assert_eq!(page.summary.lost_at_least, 1, "never above the truth");
}

#[test]
fn a_larger_loss_across_an_outage_is_a_lower_bound_never_above_the_truth() {
    let rig = outage(5);
    let page = read_durable(&rig, Some(1000), 6000);
    assert!(page.summary.lost_at_least >= 4 && page.summary.lost_at_least <= 5);
    assert!(page.gaps.iter().all(|g| g.count_is_lower_bound));
}

#[test]
fn a_pair_broken_apart_says_it_may_overcount() {
    let rig = outage(3);
    rig.engine
        .connection()
        .execute(
            "DELETE FROM gaps WHERE cause = 'upstream-loss' AND start_ms >= 4000",
            [],
        )
        .unwrap();
    let page = read_durable(&rig, Some(1000), 6000);
    assert_eq!(page.summary.may_overcount_by_pairs, 1);
    assert!(page.gaps.iter().all(|g| !g.part_of_pair));
}

#[test]
fn a_session_that_crashed_open_does_not_claim_the_time_after_its_watermark() {
    let rig = durable_rig(1000);
    let mut rig = rig;
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(3000);
    // The process is killed here: the session row has no end, the watermark
    // is wherever the last commit left it, and nothing is running now.
    let page = read_durable(&rig, None, 9000);
    let tail: Vec<_> = page.coverage.iter().filter(|s| s.end_ms > 1000).collect();
    assert!(
        tail.iter().any(|s| s.kind == Kind::Unexplained),
        "after the watermark nothing recorded: {:?}",
        page.coverage
    );
    assert!(page.sessions.iter().all(|s| !s.running));
    // Another session being live does not make this open one run to the head.
    let other = read_durable(&rig, Some(9_999), 9000);
    assert!(
        other.coverage.iter().any(|s| s.kind == Kind::Unexplained),
        "{:?}",
        other.coverage
    );
    // The same open session as the live one runs to the head.
    let live = read_durable(&rig, Some(1000), 9000);
    assert!(
        live.coverage.iter().all(|s| s.kind == Kind::Recorded),
        "{:?}",
        live.coverage
    );
    assert!(live.sessions[0].running);
}

#[test]
fn after_the_restart_a_crashed_session_is_covered_by_an_uncertain_gap() {
    let mut rig = durable_rig(1000);
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(3000);
    // Killed; the next start records the crash and opens a session.
    rig.engine
        .begin_session(&rig.registry, &rig.limits)
        .unwrap();
    rig.set(5000);
    let page = read_durable(&rig, Some(3000), 5000);
    assert_tiled_and_explained(&page);
    assert!(page.gaps.iter().any(|g| !g.start_exact), "{:?}", page.gaps);
}

#[test]
fn standing_labels_describe_the_current_session_and_say_so() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(500);
    fx.standing = Some(StandingLabels {
        nolog: true,
        stock_default_unobservable: true,
        no_gui_prompts: false,
    });
    fx.events([1]);
    let page = fx.page(None, 10);
    let s = &page.standing;
    assert_eq!(s.scope, "current_session");
    assert_eq!(s.session_start_ms, Some(500));
    assert_eq!(s.nolog, Label::Yes);
    assert_eq!(s.stock_default_unobservable, Label::Yes);
    assert_eq!(s.no_gui_prompts, Label::No);
    assert_eq!(
        s.earlier_sessions, "unknown",
        "not false: they are not persisted"
    );
}

#[test]
fn with_no_capture_running_the_labels_are_unavailable_not_false() {
    let fx = Fx::new();
    fx.events([1]);
    let s = fx.page(None, 10).standing;
    assert_eq!(s.nolog, Label::Unavailable);
    assert_eq!(s.stock_default_unobservable, Label::Unavailable);
    assert_eq!(s.no_gui_prompts, Label::Unavailable);
    assert_eq!(s.session_start_ms, None);
}

fn event_of(fx: &Fx, ts: i64, process: &str) {
    let mut e = EventRecord::new(ts, Action::Allow, AnsweredBy::Rule);
    e.process_path = Some(ProcessPath::new(process));
    insert_event(fx.conn(), &e).unwrap();
}

#[test]
fn a_process_marker_over_other_processes_events_is_never_shown_as_empty() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 1_000;
    fx.session(1);
    for ts in 1..=400 {
        event_of(
            &fx,
            ts,
            if ts % 2 == 0 {
                "/usr/bin/a"
            } else {
                "/usr/bin/b"
            },
        );
    }
    while super::retention_support::count_of(&fx.rig, "/usr/bin/a") > 0 {
        evict_process(&mut fx.rig, "/usr/bin/a").unwrap();
    }
    let page = fx.page(None, 200);
    let marker = page
        .gaps
        .iter()
        .find(|g| g.cause == "evicted")
        .expect("the marker");
    assert!(marker.overlaps_events, "b's events survive inside a's span");
    assert!(marker.count_is_lower_bound);
    assert!(page.coverage.iter().all(|s| s.kind != Kind::Unexplained));
    let evicted_seg = page
        .coverage
        .iter()
        .find(|s| s.kind == Kind::Gap && !s.gaps.is_empty())
        .unwrap();
    assert!(
        evicted_seg.session,
        "a session ran there: events may be missing, not 'nothing'"
    );
}

#[test]
fn issue_430_a_merged_prefix_and_process_marker_is_returned_as_stored() {
    // KNOWN LIMIT (#430): a prefix marker and a process marker can merge at
    // ts 0, so the row spans events that survive. The reader says what the
    // row says: "some events in this span were removed", over events it also
    // returns, and never "empty".
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 1_000;
    fx.session(1);
    fx.events(100..=300);
    fx.gap(0, 500, GapCause::Evicted, 3);
    let page = fx.page(None, 200);
    let marker = page.gaps.iter().find(|g| g.cause == "evicted").unwrap();
    assert_eq!((marker.start_ms, marker.end_ms), (0, 500));
    assert!(marker.overlaps_events);
    assert!(!page.events.is_empty());
}

#[test]
fn what_lies_below_the_floor_and_outside_the_ledger_is_named_not_blank() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(600);
    fx.now_ms = 700;
    fx.session(100);
    fx.end_session(200, super::session::SessionEnd::Clean);
    fx.session(600);
    fx.events(600..=700);
    // The bridge-down row that explained 200..600 is gone (collapsed by the
    // ledger's ceiling, say): below the floor, the stretch is named, not blank.
    fx.conn().execute("DELETE FROM gaps", []).unwrap();
    let page = fx.page(None, 200);
    assert!(
        page.coverage
            .iter()
            .any(|s| s.kind == Kind::BeforeFloor && s.start_ms == 200 && s.end_ms == 600),
        "{:?}",
        page.coverage
    );
    assert!(page.coverage.iter().all(|s| s.kind != Kind::Unexplained));
}

#[test]
fn gap_counts_are_always_lower_bounds_and_uncertain_counts_restarts() {
    let fx = Fx::new();
    fx.gap(10, 20, GapCause::Uncertain, 4);
    fx.gap(30, 40, GapCause::QueueOverflow, 7);
    let page = fx.page(None, 10);
    assert!(page.gaps.iter().all(|g| g.count_is_lower_bound));
    assert_eq!(
        page.summary.lost_at_least, 7,
        "restarts are not lost events"
    );
}

#[test]
fn a_gap_wider_than_the_window_is_clipped_for_display_only() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 1_000;
    fx.session(1);
    fx.events(1..=300);
    fx.gap(0, 5_000, GapCause::ClockJump, 2);
    let page = fx.page(None, 100);
    let g = page.gaps.iter().find(|g| g.cause == "clock-jump").unwrap();
    assert!(g.clipped, "it runs past the window");
    assert_eq!(
        (g.start_ms, g.end_ms, g.count_min),
        (0, 5_000, 2),
        "its own bounds, never shrunk"
    );
    let tiny = page.coverage.iter().find(|s| !s.gaps.is_empty()).unwrap();
    assert!(tiny.start_ms >= page.window_lo_ms);
    // A gap inside the window is not clipped.
    fx.gap(150, 160, GapCause::UpstreamLoss, 1);
    let page = fx.page(None, 200);
    let inside = page
        .gaps
        .iter()
        .find(|g| g.cause == "upstream-loss")
        .unwrap();
    assert!(!inside.clipped);
}

#[test]
fn an_eviction_between_the_events_and_the_ledger_shows_as_a_marker_not_a_hole() {
    let mut fx = Fx::new();
    fx.live_session_start_ms = Some(1);
    fx.now_ms = 1_000;
    fx.session(1);
    fx.events(1..=300);
    // Fields borrowed one by one, so the rig stays free for the eviction.
    let location = fx.rig.location.clone();
    let ctx = Context {
        location: &location,
        generation: &fx.generation,
        cancel: &fx.cancel,
        now_ms: fx.now_ms,
        live_session_start_ms: fx.live_session_start_ms,
        standing: fx.standing,
        limits: fx.limits,
    };
    let request = Request {
        cursor: None,
        limit: 200,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    // Retention removes the oldest rows after this reader took its events.
    loop {
        let older: i64 = fx
            .rig
            .engine
            .connection()
            .query_row("SELECT count(*) FROM events WHERE ts_ms < 150", [], |r| {
                r.get(0)
            })
            .unwrap();
        if older == 0 {
            break;
        }
        super::retention_support::evict_before(&mut fx.rig, 150).unwrap();
    }
    read.load_ledger().unwrap_or_else(|o| panic!("{o:?}"));
    let Outcome::Page(page) = read.finish() else {
        panic!("a page")
    };
    assert!(
        page.coverage.iter().all(|s| s.kind != Kind::Unexplained),
        "{:?}",
        page.coverage
    );
    assert!(page.gaps.iter().any(|g| g.cause == "evicted"));
}
