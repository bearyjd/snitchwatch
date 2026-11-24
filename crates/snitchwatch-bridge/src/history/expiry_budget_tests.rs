//! Finding F1: the age-expiry budget. A clock stepped forward makes every
//! event old; the bucket (12.5% of the events, refilling over 24 hours of the
//! bridge's own uptime, kept in `meta`) bounds what tier 1 removes anyway.
//! The clock in these tests is frozen and moved by hand, so the numbers are
//! exact.

use std::time::Duration;

use super::budget::Budget;
use super::record::Action;
use super::retention_support::*;
use super::writer::{Engine, WriteError};
use super::{open_store, Location};

const DAY_MS: i64 = 86_400_000;
/// A wall clock a year past the events.
const NOW: i64 = 400 * DAY_MS;
const PASS: Duration = Duration::from_secs(600);

/// `old` events from 1970 and one current one (so every old row is expired at
/// `NOW`), a `meta` row to hold the debt, and a clock that stands still.
fn store(old: i64) -> Rig {
    let mut rig = rig();
    seed_meta(&rig);
    rig.engine.freeze_expiry_clock();
    let stamps: Vec<i64> = (0..old).map(|i| 1_000 + i).collect();
    put(
        &mut rig,
        stamps
            .iter()
            .map(|ts| plain(event(*ts, "/p", Action::Allow)))
            .collect(),
    );
    put(&mut rig, vec![plain(event(NOW, "/p", Action::Allow))]);
    rig
}

fn seed_meta(rig: &Rig) {
    rig.engine
        .connection()
        .execute_batch("INSERT INTO meta (id, watermark_ms) VALUES (1, 1)")
        .unwrap();
}

fn pass(rig: &mut Rig, wall_ms: i64) -> u64 {
    rig.engine.expire_aged(wall_ms, false).unwrap().removed
}

fn stored_debt(rig: &Rig) -> i64 {
    rig.engine
        .connection()
        .query_row("SELECT expiry_debt_milli FROM meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn reopen(rig: &mut Rig) {
    let location: &Location = &rig.location;
    let store = open_store(location).unwrap();
    let budget = Budget::new(16 * 1024 * 1024).unwrap();
    let engine = Engine::new(store, location, budget).unwrap();
    // The old engine's connection closes when it is replaced.
    rig.engine = engine;
    rig.engine.freeze_expiry_clock();
}

#[test]
fn fifty_passes_on_a_clock_a_year_ahead_leave_at_least_85_percent() {
    let mut rig = store(1000);
    for _ in 0..50 {
        pass(&mut rig, NOW);
    }
    let left = count(&rig, "events");
    assert!(left * 100 >= 1001 * 85, "{left} of 1001 left");
    assert_eq!(
        left, 876,
        "an eighth of 1001 is 125, no refill without uptime"
    );
}

#[test]
fn fifty_passes_ten_minutes_apart_leave_what_an_eighth_plus_eight_hours_of_refill_leave() {
    let mut rig = store(1000);
    for _ in 0..50 {
        pass(&mut rig, NOW);
        rig.engine.advance_expiry_clock(PASS);
    }
    assert_eq!(count(&rig, "events"), 839, "83.8% after eight hours");
}

#[test]
fn a_wall_clock_stepped_a_year_forward_refills_nothing() {
    let mut rig = store(1000);
    assert_eq!(pass(&mut rig, NOW), 100, "the per-pass tenth");
    assert_eq!(pass(&mut rig, NOW), 25, "what is left of the eighth");
    assert_eq!(pass(&mut rig, NOW), 0, "empty");
    assert_eq!(
        pass(&mut rig, NOW + 400 * DAY_MS),
        0,
        "a further year forward"
    );
    assert_eq!(pass(&mut rig, i64::MAX / 2), 0, "and the end of time");
    assert_eq!(count(&rig, "events"), 876);
}

#[test]
fn an_idle_store_expires_over_days_on_this_exact_schedule() {
    // 1000 expired events, one current, a pass every ten minutes of uptime.
    // Each row is what remains at the end of that day (144 passes a day). The
    // schedule is the integer model in the plan: a bucket of (events + debt)/8
    // refilling in 24 hours.
    let mut rig = store(1000);
    let mut left = Vec::new();
    for _ in 0..4 {
        for _ in 0..144 {
            pass(&mut rig, NOW);
            rig.engine.advance_expiry_clock(PASS);
        }
        left.push(count(&rig, "events"));
    }
    assert_eq!(left, [768, 671, 586, 511]);
}

#[test]
fn a_small_store_still_expires_whole_in_one_pass() {
    let mut rig = store(10);
    assert_eq!(pass(&mut rig, NOW), 10, "never under one chunk of rows");
    assert_eq!(count(&rig, "events"), 1);
}

#[test]
fn a_restart_keeps_the_debt_and_does_not_refill() {
    let mut rig = store(1000);
    assert_eq!(pass(&mut rig, NOW), 100);
    assert_eq!(pass(&mut rig, NOW), 25);
    assert_eq!(stored_debt(&rig), 125_000);
    reopen(&mut rig);
    assert_eq!(
        rig.engine.expiry_debt_milli(),
        125_000,
        "the debt is read back"
    );
    assert_eq!(pass(&mut rig, NOW), 0, "the bucket is still empty");
    assert_eq!(
        pass(&mut rig, NOW + 400 * DAY_MS),
        0,
        "whatever the clock says"
    );
    // And the new process's own uptime refills it.
    rig.engine.advance_expiry_clock(Duration::from_secs(3600));
    assert!(pass(&mut rig, NOW) > 0);
}

#[test]
fn the_debit_is_in_the_deleting_transaction() {
    let mut rig = store(1000);
    rig.engine.fault_before_commit_for_test(Box::new(|| {
        Some(WriteError::Io(std::io::Error::other("x")))
    }));
    assert!(rig.engine.expire_aged(NOW, false).is_err());
    assert_eq!(count(&rig, "events"), 1001, "nothing was removed");
    assert_eq!(stored_debt(&rig), 0, "and nothing was spent");
    assert_eq!(rig.engine.expiry_debt_milli(), 0);
}

#[test]
fn a_chunk_that_committed_is_debited_with_its_rows() {
    let mut rig = store(1000);
    assert_eq!(pass(&mut rig, NOW), 100);
    assert_eq!(stored_debt(&rig), 100_000, "in thousandths of a row");
}

#[test]
fn a_debt_larger_than_the_bucket_is_cut_to_it() {
    let mut rig = store(1000);
    rig.engine
        .connection()
        .execute("UPDATE meta SET expiry_debt_milli = ?1", [i64::MAX])
        .unwrap();
    reopen(&mut rig);
    assert_eq!(pass(&mut rig, NOW), 0, "empty at first");
    rig.engine
        .advance_expiry_clock(Duration::from_secs(24 * 3600));
    assert_eq!(pass(&mut rig, NOW), 100, "a day of uptime refills it whole");
}

#[test]
fn a_skipped_pass_spends_nothing() {
    let mut rig = store(1000);
    let skipped = rig.engine.expire_aged(NOW, true).unwrap();
    assert!(skipped.skipped_for_jump);
    assert_eq!(stored_debt(&rig), 0);
    assert_eq!(
        pass(&mut rig, NOW),
        100,
        "the next pass has the whole bucket"
    );
}

#[test]
fn the_real_clock_refills_between_passes() {
    let mut rig = rig();
    seed_meta(&rig);
    put(
        &mut rig,
        (0..1000)
            .map(|i| plain(event(1_000 + i, "/p", Action::Allow)))
            .collect(),
    );
    assert_eq!(pass(&mut rig, NOW), 100);
    let before = rig.engine.expiry_debt_milli();
    std::thread::sleep(Duration::from_millis(1000));
    assert!(rig.engine.expiry_debt_milli() < before, "uptime refills it");
}

/// `per_hour` events an hour for `days` days, an expiry pass every hour, and
/// the bridge up (its monotonic clock running) for `up_hours` of every 24.
/// Returns the age in days of the oldest event left and the event count.
fn simulate(retention: u16, per_hour: i64, days: i64, up_hours: i64) -> (f64, i64) {
    let mut rig = rig();
    seed_meta(&rig);
    rig.engine.freeze_expiry_clock();
    rig.engine.set_retention_days(retention);
    let hour_ms = DAY_MS / 24;
    for hour in 0..days * 24 {
        let wall = hour * hour_ms;
        let items = (0..per_hour).map(|i| plain(event(wall + i, "/p", Action::Allow)));
        put(&mut rig, items.collect());
        pass(&mut rig, wall + per_hour);
        if hour % 24 < up_hours {
            rig.engine.advance_expiry_clock(Duration::from_secs(3600));
        }
    }
    let oldest: i64 = rig
        .engine
        .connection()
        .query_row("SELECT min(ts_ms) FROM events", [], |r| r.get(0))
        .unwrap();
    let age = ((days * 24 * hour_ms - oldest) as f64) / DAY_MS as f64;
    (age, count(&rig, "events"))
}

#[test]
fn every_retention_option_keeps_up_with_its_inflow_at_full_uptime() {
    for retention in [7_u16, 30, 90] {
        let window = f64::from(retention);
        let (age, events) = simulate(retention, 4, i64::from(retention) * 4 + 30, 24);
        assert!(
            age <= window * 1.1,
            "R={retention}: oldest is {age} days old"
        );
        assert!(
            events as f64 <= 96.0 * window * 1.1,
            "R={retention}: {events}"
        );
    }
}

#[test]
fn at_half_uptime_a_seven_day_store_settles_a_quarter_over_its_window() {
    // Refill per day of wall time is halved, so the store grows until the
    // bucket (the debt counts as stored rows) matches the inflow: measured,
    // 8.8 days for a 7-day window (13.5 days without the retention floor).
    let (age, _) = simulate(7, 4, 90, 12);
    assert!((8.0..=9.6).contains(&age), "settled at {age} days");
}

#[test]
fn a_year_forward_clock_on_a_seven_day_store_leaves_over_eighty_percent() {
    let mut rig = store(1000);
    rig.engine.set_retention_days(7);
    assert_eq!(pass(&mut rig, NOW), 100, "the per-pass tenth");
    for _ in 0..50 {
        pass(&mut rig, NOW);
    }
    let left = count(&rig, "events");
    assert!(left * 100 >= 1001 * 80, "{left} of 1001 left");
    assert_eq!(left, 822, "ceil(1.25 * 1001 / 7) = 179 rows, no refill");
}

#[test]
fn the_bucket_of_a_ninety_day_store_stays_an_eighth() {
    let mut rig = store(1000);
    rig.engine.set_retention_days(90);
    for _ in 0..50 {
        pass(&mut rig, NOW);
    }
    assert_eq!(
        count(&rig, "events"),
        876,
        "unchanged: the eighth is larger"
    );
}

#[test]
fn the_debit_is_visible_to_other_connections_the_moment_the_delete_is() {
    // The observer runs right after the deleting COMMIT. A debit written in a
    // second transaction would not be there yet.
    let mut rig = store(1000);
    let path = rig.location.db_path();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    rig.engine.set_observer(Box::new(move |report| {
        let other = rusqlite::Connection::open(&path).unwrap();
        let row = |sql: &str| other.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        let events = row("SELECT count(*) FROM events");
        let debt = row("SELECT expiry_debt_milli FROM meta WHERE id = 1");
        log.lock().unwrap().push((report.kind, events, debt));
    }));
    assert_eq!(pass(&mut rig, NOW), 100);
    let seen = seen.lock().unwrap();
    let deletes: Vec<_> = seen
        .iter()
        .filter(|(kind, _, _)| *kind == super::writer::TxnKind::Evict)
        .collect();
    assert!(!deletes.is_empty());
    for (_, events, debt) in deletes {
        assert_eq!(
            *debt,
            (1001 - events) * 1000,
            "{events} events, debt {debt}"
        );
    }
}
