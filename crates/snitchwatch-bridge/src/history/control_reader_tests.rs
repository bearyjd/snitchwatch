//! PR 10: the real reader against the gate. A cursor from before a clear or
//! a disable/re-enable is refused, and what a clear carries is delivered
//! without ever being presented as recorded coverage.

use super::control::Limits as GateLimits;
use super::control_support::consent;
use super::control_tests::{feed, gate_rig, wait_for, GateRig};
use super::reader::{read_page, Context, Cursor, Limits, Outcome, Page, Request};
use super::reader_registry::CancelToken;
use super::timeline::Kind;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn read(rig: &GateRig, cursor: Option<Cursor>, limit: usize) -> Outcome {
    let generation = rig.gate.generation();
    let cancel = CancelToken::default();
    let ctx = Context {
        location: &rig.location,
        generation: &generation,
        cancel: &cancel,
        now_ms: now_ms(),
        live_session_start_ms: None,
        standing: None,
        limits: Limits::default(),
    };
    read_page(&Request { cursor, limit }, &ctx)
}

fn page(rig: &GateRig, cursor: Option<Cursor>, limit: usize) -> Page {
    match read(rig, cursor, limit) {
        Outcome::Page(page) => *page,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn rows(rig: &GateRig) -> i64 {
    super::control_support::count(&rig.location, "SELECT COUNT(*) FROM events")
}

#[test]
fn a_cursor_from_before_a_clear_is_refused() {
    let rig = gate_rig(GateLimits::default());
    feed(&rig, 3);
    wait_for("rows", || rows(&rig) == 3);
    let first = page(&rig, None, 2);
    let cursor = first.next_cursor.expect("more to read");
    assert!(matches!(read(&rig, Some(cursor), 2), Outcome::Page(_)));
    let token = rig.gate.preview().unwrap().token;
    rig.gate.clear(token).unwrap();
    assert!(matches!(
        read(&rig, Some(cursor), 2),
        Outcome::StaleGeneration
    ));
    let fresh = page(&rig, None, 2);
    assert_eq!(fresh.generation, rig.gate.generation().current());
    assert!(fresh.events.is_empty());
}

#[test]
fn a_cursor_from_before_a_disable_and_reenable_is_refused() {
    let rig = gate_rig(GateLimits::default());
    feed(&rig, 3);
    wait_for("rows", || rows(&rig) == 3);
    let cursor = page(&rig, None, 2).next_cursor.expect("more to read");
    rig.gate.disable().unwrap();
    rig.gate.enable(consent()).unwrap();
    assert!(matches!(
        read(&rig, Some(cursor), 2),
        Outcome::StaleGeneration
    ));
    assert_eq!(page(&rig, None, 5).events.len(), 3, "history was kept");
}

#[test]
fn what_a_clear_carries_is_delivered_and_never_shown_as_recorded() {
    let rig = gate_rig(GateLimits::default());
    feed(&rig, 2);
    wait_for("rows", || rows(&rig) == 2);
    let token = rig.gate.preview().unwrap().token;
    feed(&rig, 1);
    wait_for("the later row", || rows(&rig) == 3);
    rig.gate.clear(token).unwrap();
    let page = page(&rig, None, 10);
    assert_eq!(page.events.len(), 1, "the carried event is delivered");
    let at = page.events[0].ts_ms;
    for segment in page
        .coverage
        .iter()
        .filter(|s| s.start_ms <= at && at <= s.end_ms)
    {
        assert!(
            !matches!(segment.kind, Kind::Recorded) && !segment.session,
            "the carried instant is not recorded coverage: {segment:?}"
        );
    }
    assert!(page
        .coverage
        .iter()
        .any(|s| matches!(s.kind, Kind::Unexplained | Kind::BeforeFloor | Kind::Gap)));
}
