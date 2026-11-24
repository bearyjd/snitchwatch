//! PR 8: the running writer's side of retention: the expiry schedule, the
//! clock-jump skip, and the byte accounting that flows back to admission.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::capture::{Capture, CaptureConfig};
use super::durable::tests::manual_clock;
use super::gaps::ClockWatch;
use super::ledger::{read_gaps, GapCause};
use super::pins::PinRegistry;
use super::record::Action;
use super::retention::Schedule;
use super::retention_support::*;
use super::Location;

const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 400 * DAY_MS;

#[test]
fn a_pass_is_due_once_per_period_after_the_start() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Duration::from_secs(600), t0);
    assert!(
        !schedule.due(t0),
        "not at the start: a jump is given a heartbeat to be seen"
    );
    assert!(!schedule.due(t0 + Duration::from_secs(599)));
    assert!(schedule.due(t0 + Duration::from_secs(600)));
    assert!(!schedule.due(t0 + Duration::from_secs(601)));
    assert!(schedule.due(t0 + Duration::from_secs(1200)));
}

#[test]
fn a_jump_stays_flagged_until_a_pass_takes_it() {
    let mut clock = ClockWatch::new();
    clock.observe(0, 0);
    clock.observe(5_000, 5_000);
    assert!(!clock.take_jumped());
    clock.observe(5_000 + DAY_MS, 10_000); // a day in five seconds
    clock.observe(5_000 + DAY_MS + 5_000, 15_000); // quiet again
    assert!(clock.take_jumped(), "sticky across the quiet heartbeat");
    assert!(!clock.take_jumped(), "taken");
}

#[test]
fn eviction_reports_the_event_bytes_it_removed_per_process_for_admission() {
    let mut rig = rig();
    put(
        &mut rig,
        vec![
            plain(event(1, "/a", Action::Allow)),
            plain(event(2, "/b", Action::Allow)),
            plain(event(3, "/a", Action::Deny)),
        ],
    );
    assert!(rig.engine.take_released().is_empty());
    rig.engine.evict_chunk().unwrap();
    let mut released = rig.engine.take_released();
    released.sort();
    let row = row_bytes_of("/a");
    assert_eq!(
        released,
        vec![("/a".to_string(), 2 * row), ("/b".to_string(), row)]
    );
    assert!(rig.engine.take_released().is_empty(), "taken once");
}

#[test]
fn the_ledger_seed_is_each_process_s_event_bytes_without_snapshots() {
    let mut rig = rig();
    put(
        &mut rig,
        vec![
            with_rule(event(1, "/a", Action::Allow), 1),
            plain(event(2, "/b", Action::Allow)),
        ],
    );
    let mut seed = rig.engine.event_bytes().unwrap();
    seed.sort();
    let row = row_bytes_of("/a");
    assert_eq!(seed, vec![("/a".to_string(), row), ("/b".to_string(), row)]);
}

// ---- the running writer ---------------------------------------------------------

struct Live {
    _root: tempfile::TempDir,
    location: Location,
    clock: Arc<std::sync::atomic::AtomicI64>,
    capture: Capture,
}

/// A capture at wall time `NOW` over a store holding `old` rows of
/// `/old/p` at time `old_ts` and one recent row.
fn live_at(old_ts: i64, old: i64, tweak: impl FnOnce(&mut CaptureConfig)) -> Live {
    let rig = rig();
    fill(
        &rig,
        (0..old).map(|_| event(old_ts, "/old/p", Action::Allow)),
    );
    fill(&rig, [event(NOW, "/new/p", Action::Allow)]);
    let Rig {
        _root,
        location,
        engine,
        ..
    } = rig;
    let (clock, read) = manual_clock(NOW);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let mut config = CaptureConfig::for_budget(engine.budget());
    config.heartbeat_every = Duration::from_millis(10);
    config.expiry_every = Duration::from_millis(100);
    tweak(&mut config);
    let capture = Capture::spawn(engine, registry, config).unwrap();
    Live {
        _root,
        location,
        clock,
        capture,
    }
}

fn live(old: i64, tweak: impl FnOnce(&mut CaptureConfig)) -> Live {
    live_at(1_000, old, tweak)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn db_events(location: &Location) -> i64 {
    rusqlite::Connection::open(location.db_path())
        .unwrap()
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_writer_seeds_admission_from_the_database_then_expires_and_releases() {
    let live = live(4, |c| c.expiry_every = Duration::from_millis(300));
    let row = row_bytes_of("/old/p");
    wait_for("the seed", || {
        live.capture.retained_for_test("/old/p") == 4 * row
    });
    wait_for("the expiry pass", || db_events(&live.location) == 1);
    wait_for("the release", || {
        live.capture.retained_for_test("/old/p") == 0
    });
    let gaps = read_gaps(&rusqlite::Connection::open(live.location.db_path()).unwrap()).unwrap();
    assert!(
        gaps.iter().any(|g| g.cause == GapCause::Evicted
            && g.start_ms <= 1_000
            && 1_000 <= g.end_ms
            && g.count == 4),
        "{gaps:?}"
    );
    assert_eq!(live.capture.stats().expiry_skipped, 0);
}

#[test]
fn a_pass_after_a_clock_jump_is_skipped_and_the_next_one_goes_ahead() {
    let live = live(4, |c| c.expiry_every = Duration::from_millis(800));
    // The writer takes its clock baseline as it starts; the jump comes after.
    std::thread::sleep(Duration::from_millis(250));
    live.clock.store(NOW + 365 * DAY_MS, Ordering::SeqCst);
    wait_for("the skipped pass", || {
        live.capture.stats().expiry_skipped == 1
    });
    assert_eq!(
        db_events(&live.location),
        5,
        "nothing was expired by the jumped pass"
    );
    wait_for("the next pass", || db_events(&live.location) < 5);
}

#[test]
fn the_capture_expires_by_the_retention_it_is_configured_with() {
    // Eight days old: kept at the default 30 days, expired at 7.
    let eight_days_ago = NOW - 8 * DAY_MS;
    let kept = live_at(eight_days_ago, 4, |_| {});
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(db_events(&kept.location), 5, "30 days keeps them");
    let expired = live_at(eight_days_ago, 4, |c| c.retention_days = 7);
    wait_for("the 7-day pass", || db_events(&expired.location) == 1);
}

#[test]
fn the_users_retention_setting_reaches_the_capture_config() {
    let budget = super::budget::Budget::new(16 * 1024 * 1024).unwrap();
    for days in [7_u16, 30, 90] {
        let settings = super::settings::Settings {
            retention_days: days,
            ..Default::default()
        };
        assert_eq!(
            CaptureConfig::for_settings(&budget, &settings).retention_days,
            days
        );
    }
}
