use super::*;

// N3 (plan `2026-10-09-n3-unused-window-from-daemon-counters.md`): a bridge
// restart is judged from the daemon's counters at the first ping, against the
// baseline the previous run saved. Each test names its row of the plan's
// table.

const DAY: i64 = 86_400_000;
/// When the previous run counted its last ping.
const LAST_PING: i64 = NOW - 60_000;
/// The daemon's uptime then (seconds): it started at `LAST_PING - 3_600 s`.
const UPTIME_THEN: u64 = 3_600;
const HITS_THEN: u64 = 100;
/// The first ping of the new run, 10 s after the restore at `NOW`.
const FIRST: i64 = NOW + 10_000;
/// A daemon that stayed up: its uptime at `FIRST`.
const UPTIME_UP: u64 = UPTIME_THEN + 70;

fn baseline(ping_unix_ms: i64, uptime: u64, rule_hits: u64) -> DaemonBaseline {
    DaemonBaseline {
        ping_unix_ms,
        uptime,
        rule_hits,
    }
}

/// What the previous run saved: `a` counted 4 times, an old gap 20 days
/// back, the daemon's counters at its last ping, and a clean stop or not.
fn left_by(daemon: DaemonBaseline, clean: bool) -> Saved {
    Saved {
        last_gap_unix_ms: Some(NOW - 20 * DAY),
        daemon: Some(daemon),
        stopped_unix_ms: clean.then_some(LAST_PING + 1_000),
        ..saved(&[("a", 4)])
    }
}

fn usual(clean: bool) -> Saved {
    left_by(baseline(LAST_PING, UPTIME_THEN, HITS_THEN), clean)
}

/// A bridge on the Unix socket that restored `saved` at `NOW`.
fn restored(saved: Saved) -> RuleHits {
    let mut hits = RuleHits::default();
    hits.trust_daemon_counters(true);
    hits.restore(saved, NOW);
    hits
}

fn a_events(n: usize) -> Vec<Event> {
    (0..n).map(|i| ev("a", i as i64 + 1)).collect()
}

/// The saved gap, 20 days back: the judgement found nothing missing.
fn no_new_gap(hits: &RuleHits) {
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW - 20 * DAY));
    assert_eq!(
        hits.to_saved().unwrap().last_gap_unix_ms,
        Some(NOW - 20 * DAY)
    );
}

/// A gap at the first ping, also saved.
fn gap_at_first_ping(hits: &RuleHits) {
    assert_eq!(hits.last_gap_unix_ms(), Some(FIRST));
    assert_eq!(hits.to_saved().unwrap().last_gap_unix_ms, Some(FIRST));
}

#[test]
fn until_the_first_ping_a_restore_shows_a_provisional_gap_that_is_never_saved() {
    // Row 9: nothing reads "Unused" before the judgement.
    let hits = restored(usual(true));
    assert!(hits.is_lossy());
    assert_eq!(
        hits.last_gap_unix_ms(),
        Some(NOW),
        "provisional, on the wire"
    );
    let out = hits.to_saved().unwrap();
    assert_eq!(out.last_gap_unix_ms, Some(NOW - 20 * DAY), "the real one");
    assert_eq!(
        out.daemon,
        Some(baseline(LAST_PING, UPTIME_THEN, HITS_THEN)),
        "the next run can still judge"
    );
    assert_eq!(out.stopped_unix_ms, None, "only the shutdown save says so");
}

#[test]
fn a_daemon_that_stayed_up_and_delivered_every_hit_is_no_gap() {
    // Row 4: the hits made while the bridge was down waited in the daemon's
    // batch and came with the first ping.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    no_new_gap(&hits);
    hits.adopt_snapshot(FIRST, |n| n == "a");
    assert_eq!(counts(&hits), vec![pair("a", 6)]);
}

#[test]
fn a_daemon_that_stayed_up_with_no_previous_gap_is_not_lossy() {
    let mut hits = restored(Saved {
        last_gap_unix_ms: None,
        ..usual(true)
    });
    assert!(hits.is_lossy(), "provisional");
    let before = hits.revision();
    hits.record(&a_events(1), UPTIME_UP, HITS_THEN + 1, FIRST, |_| true);
    assert!(!hits.is_lossy());
    assert_eq!(hits.last_gap_unix_ms(), None);
    assert_ne!(hits.revision(), before, "clients are told");
}

#[test]
fn a_daemon_that_stayed_up_and_lost_hits_is_a_gap_at_the_first_ping() {
    // Row 4: more than `MaxEvents` while the bridge was down, or a failed ping.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 3, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_daemon_that_stayed_up_is_judged_by_its_counter_even_after_a_crash() {
    // Row 4 needs no clean stop: hits the crashed run received after its
    // last save are in the counter's growth and not in this ping.
    let mut hits = restored(usual(false));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    no_new_gap(&hits);
    let mut hits = restored(usual(false));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 5, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn fewer_hits_counted_than_events_received_is_a_gap() {
    // Row 8: impossible within one daemon run.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 1, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_restarted_daemon_whose_first_ping_holds_every_hit_is_no_gap() {
    // Rows 2/3 then 5: rebooted, and a clean stop before it.
    let mut hits = restored(usual(true));
    hits.record(&a_events(3), 40, 3, FIRST, |_| true);
    no_new_gap(&hits);
}

#[test]
fn a_restarted_daemon_with_more_hits_than_its_first_ping_is_a_gap() {
    // Row 5: more than `MaxEvents` before the bridge's first ping.
    let mut hits = restored(usual(true));
    hits.record(&a_events(3), 40, 4, FIRST, |_| true);
    gap_at_first_ping(&hits);
    let mut hits = restored(usual(true));
    hits.record(&a_events(3), 40, 2, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_restart_without_a_clean_stop_is_a_gap() {
    // Row 6: what the old bridge received after its last periodic save is
    // gone, and the new daemon's counter can't show it.
    let mut hits = restored(usual(false));
    hits.record(&a_events(3), 40, 3, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_counter_that_went_down_is_a_restart_even_when_the_start_time_agrees() {
    // Row 2 on its own: same start time, uptime grew, the counter fell.
    let mut hits = restored(usual(true));
    hits.record(&a_events(3), UPTIME_UP, 3, FIRST, |_| true);
    no_new_gap(&hits);
}

#[test]
fn an_uptime_that_went_down_is_a_restart_even_when_the_start_time_agrees() {
    // Row 2 on its own: the bridge's clock stepped back 2 s since the last
    // run, so the start times agree although the uptime fell.
    let first = LAST_PING - 2_000;
    let mut hits = restored(usual(true));
    hits.record(&a_events(120), UPTIME_THEN - 1, 120, first, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW - 20 * DAY));
}

#[test]
fn a_daemon_that_started_after_the_last_ping_is_a_restart() {
    // Row 3 on its own: it has run longer, and counted more, than the old
    // one had at the last ping, but it started after that ping.
    let mut hits = restored(left_by(baseline(NOW - DAY, 10, 100), true));
    hits.record(&a_events(150), 3_600, 150, FIRST, |_| true);
    no_new_gap(&hits);
}

#[test]
fn a_restart_soon_after_a_young_daemons_last_ping_is_not_taken_for_the_same_run() {
    // Row 3 before row 4: the old daemon pinged 2 s after it started, the
    // new one started 1 s after that ping, so the start times are 3 s
    // apart. Taken as the same run, `12 - 5 = 7` would hide 5 lost hits.
    let mut hits = restored(left_by(baseline(LAST_PING, 2, 5), true));
    let uptime = ((FIRST - (LAST_PING + 1_000)) / 1_000) as u64;
    hits.record(&a_events(7), uptime, 12, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_start_time_a_little_off_is_still_the_same_run() {
    // Row 4: whole-second uptime and delivery time move the estimate by up
    // to 2 s; 4 s is within the slack.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP - 4, HITS_THEN + 2, FIRST, |_| true);
    no_new_gap(&hits);
}

#[test]
fn a_start_time_that_moved_but_not_past_the_last_ping_is_a_gap() {
    // Row 7: a suspend while the daemon kept running (its uptime stops) or a
    // clock step. Neither the same run nor provably a restart.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP - 6, HITS_THEN + 2, FIRST, |_| true);
    gap_at_first_ping(&hits);
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP + 6, HITS_THEN + 2, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_daemon_that_stayed_up_but_looks_restarted_is_a_gap_when_it_had_hits() {
    // The safety argument: a long suspend pushes the start estimate past the
    // last ping, so the stricter restart test applies, and the old run's
    // hits are in the counter.
    let mut hits = restored(left_by(baseline(LAST_PING, 10, HITS_THEN), true));
    hits.record(&a_events(2), 15, HITS_THEN + 2, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn only_default_applied_events_in_the_first_ping() {
    // Row 10: they grow `rule_misses`, not `rule_hits`.
    let mut hits = restored(usual(true));
    hits.record(&[marked(1), marked(2)], UPTIME_UP, HITS_THEN, FIRST, |_| {
        true
    });
    no_new_gap(&hits);
    let mut hits = restored(usual(true));
    hits.record(&[marked(1)], 40, 0, FIRST, |_| true);
    no_new_gap(&hits);
    let mut hits = restored(usual(true));
    hits.record(&[marked(1)], 40, 1, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_file_without_a_baseline_cannot_be_judged_and_is_a_gap_at_once() {
    // Row 1: a version 1 file, or one a TCP bridge saved.
    let mut hits = restored(saved(&[("a", 4)]));
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
    assert_eq!(hits.to_saved().unwrap().last_gap_unix_ms, Some(NOW), "real");
    hits.record(&a_events(1), 40, 1, FIRST, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW), "nothing undoes it");
}

#[test]
fn over_tcp_a_restore_is_a_gap_at_once_and_no_baseline_is_saved() {
    // Row 11: anyone local can send the first ping.
    let mut hits = RuleHits::default();
    hits.restore(usual(true), NOW);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
    assert_eq!(hits.to_saved().unwrap().last_gap_unix_ms, Some(NOW));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW), "not undone");
    assert_eq!(hits.to_saved().unwrap().daemon, None);
}

#[test]
fn the_baseline_saved_is_the_last_ping_counted() {
    let mut hits = RuleHits::default();
    hits.trust_daemon_counters(true);
    assert_eq!(hits.to_saved(), None);
    hits.record(&a_events(1), 50, 7, NOW, |_| true);
    hits.record(&a_events(2), 51, 9, NOW + 1_000, |_| true);
    let out = hits.to_saved().unwrap();
    assert_eq!(out.daemon, Some(baseline(NOW + 1_000, 51, 9)));
    assert_eq!(out.stopped_unix_ms, None);
}

#[test]
fn after_the_judgement_the_first_ping_is_the_baseline() {
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    assert_eq!(
        hits.to_saved().unwrap().daemon,
        Some(baseline(FIRST, UPTIME_UP, HITS_THEN + 2))
    );
    hits.record(
        &a_events(1),
        UPTIME_UP + 1,
        HITS_THEN + 3,
        FIRST + 1_000,
        |_| true,
    );
    no_new_gap(&hits);
    hits.record(
        &a_events(1),
        UPTIME_UP + 2,
        HITS_THEN + 9,
        FIRST + 2_000,
        |_| true,
    );
    assert_eq!(
        hits.last_gap_unix_ms(),
        Some(FIRST + 2_000),
        "in-run, as before"
    );
}

#[test]
fn a_real_gap_while_waiting_for_the_first_ping_stays() {
    let mut hits = restored(usual(true));
    hits.adopt_snapshot(NOW + 1_000, |n| n == "a");
    // The daemon's next list lacks `a`: its count is gone, a real gap.
    hits.adopt_snapshot(NOW + 2_000, |_| false);
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 2_000));
    assert_eq!(hits.to_saved().unwrap().last_gap_unix_ms, Some(NOW + 2_000));
}

#[test]
fn a_run_with_no_ping_hands_the_old_baseline_on() {
    // Row 9: the next run judges from what the last ping showed.
    let mut hits = restored(usual(true));
    hits.adopt_snapshot(NOW + 1_000, |n| n == "a");
    let out = hits.to_saved().unwrap();
    assert_eq!(
        out.daemon,
        Some(baseline(LAST_PING, UPTIME_THEN, HITS_THEN))
    );
    let mut next = restored(out);
    next.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    no_new_gap(&next);
}
