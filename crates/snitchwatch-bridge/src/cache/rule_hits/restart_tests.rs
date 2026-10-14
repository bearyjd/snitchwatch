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

/// These tests are about which counter says "restart", not the clean stop's
/// window: the stop is put a second before the daemon's start, `uptime`
/// seconds before `first`, so it vouches.
fn stopped_before_the_daemon_started(saved: Saved, first: i64, uptime: u64) -> Saved {
    Saved {
        stopped_unix_ms: Some(first - secs_to_ms(uptime) - 1_000),
        ..saved
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
    assert_eq!(
        out.stopped_unix_ms, None,
        "a clean stop is consumed when read: only the shutdown save writes one"
    );
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
    let mut hits = restored(stopped_before_the_daemon_started(
        usual(true),
        FIRST,
        UPTIME_UP,
    ));
    hits.record(&a_events(3), UPTIME_UP, 3, FIRST, |_| true);
    no_new_gap(&hits);
}

#[test]
fn an_uptime_that_went_down_is_a_restart_even_when_the_start_time_agrees() {
    // Row 2 on its own: the bridge's clock stepped back 2 s since the last
    // run, so the start times agree although the uptime fell.
    let first = LAST_PING - 2_000;
    let mut hits = restored(stopped_before_the_daemon_started(
        usual(true),
        first,
        UPTIME_THEN - 1,
    ));
    hits.record(&a_events(120), UPTIME_THEN - 1, 120, first, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW - 20 * DAY));
}

#[test]
fn a_daemon_that_started_after_the_last_ping_is_a_restart() {
    // Row 3 on its own: it has run longer, and counted more, than the old
    // one had at the last ping, but it started after that ping.
    let mut hits = restored(left_by(baseline(NOW - DAY, 10, 100), true));
    hits.record(&a_events(150), 40, 150, FIRST, |_| true);
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

#[test]
fn a_run_with_no_ping_keeps_the_previous_runs_stop_status() {
    // Row 9: a run that counted nothing can't vouch for the run before it.
    // The previous run crashed: its last events may be lost, and a clean stop
    // now must not hide that from a run after a reboot.
    let hits = restored(usual(false));
    assert_eq!(hits.to_saved().unwrap().stopped_unix_ms, None);
    let out = hits.to_saved_at_stop(NOW + 5_000).unwrap();
    assert_eq!(out.stopped_unix_ms, None, "the crash stands");
    let mut next = restored(out);
    next.record(&a_events(3), 40, 3, FIRST, |_| true);
    gap_at_first_ping(&next);
    // The previous run stopped cleanly: its shutdown save says so again (a
    // periodic save never does: the stop was consumed when it was read).
    let hits = restored(usual(true));
    assert_eq!(hits.to_saved().unwrap().stopped_unix_ms, None);
    let out = hits.to_saved_at_stop(NOW + 5_000).unwrap();
    assert_eq!(out.stopped_unix_ms, Some(LAST_PING + 1_000));
    let mut next = restored(out);
    next.record(&a_events(3), 40, 3, FIRST, |_| true);
    no_new_gap(&next);
}

#[test]
fn once_judged_the_shutdown_save_is_this_runs_clean_stop() {
    let mut hits = restored(usual(false));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    assert_eq!(hits.to_saved().unwrap().stopped_unix_ms, None);
    assert_eq!(
        hits.to_saved_at_stop(FIRST + 5_000)
            .unwrap()
            .stopped_unix_ms,
        Some(FIRST + 5_000)
    );
    // A run that restored nothing.
    let mut fresh = RuleHits::default();
    fresh.trust_daemon_counters(true);
    assert_eq!(fresh.to_saved_at_stop(NOW), None, "counting hasn't started");
    fresh.record(&a_events(1), 50, 7, NOW, |_| true);
    assert_eq!(
        fresh.to_saved_at_stop(NOW + 1).unwrap().stopped_unix_ms,
        Some(NOW + 1)
    );
}

// Review of PR #123.

#[test]
fn exactly_at_the_slack_the_start_time_is_still_the_same_run() {
    // Row 4's bound is inclusive, both ways.
    for uptime in [UPTIME_UP - 5, UPTIME_UP + 5] {
        let mut hits = restored(usual(true));
        hits.record(&a_events(2), uptime, HITS_THEN + 2, FIRST, |_| true);
        no_new_gap(&hits);
    }
}

#[test]
fn a_start_exactly_the_slack_before_the_last_ping_is_a_restart() {
    // Row 3's bound is inclusive, and wins over row 4: the old daemon pinged
    // 10 s after its start, the start estimate now is 5 s before that ping
    // (5 s from the old start too). As the same run, `H - H₀ = R` would pass.
    let mut hits = restored(left_by(baseline(LAST_PING, 10, HITS_THEN), true));
    let uptime = ((FIRST - (LAST_PING - 5_000)) / 1_000) as u64;
    hits.record(&a_events(2), uptime, HITS_THEN + 2, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn hits_waiting_for_the_first_snapshot_are_saved_with_the_baseline() {
    // The stock daemon pings before its HELLO is committed, so that batch
    // waits in the side map: saved, the next run's first snapshot sorts it.
    let mut hits = RuleHits::default();
    hits.trust_daemon_counters(true);
    hits.record(
        &[ev("a", 1), ev("a", 2), ev("once-1", 3)],
        50,
        7,
        NOW,
        |_| false,
    );
    let out = hits.to_saved_at_stop(NOW + 1_000).unwrap();
    let mut got: Vec<_> = out
        .hits
        .iter()
        .map(|h| (h.name.as_str(), h.count))
        .collect();
    got.sort();
    assert_eq!(got, vec![("a", 2), ("once-1", 1)]);
    assert_eq!(out.daemon, Some(baseline(NOW, 50, 7)));
    let mut next = RuleHits::default();
    next.trust_daemon_counters(true);
    next.restore(out, NOW + 2_000);
    next.adopt_snapshot(NOW + 3_000, |n| n == "a");
    next.record(&[ev("a", 4)], 52, 8, NOW + 4_000, |n| n == "a");
    assert_eq!(counts(&next), vec![pair("a", 3)], "once-1 filtered out");
    assert!(!next.is_lossy(), "gap at {:?}", next.last_gap_unix_ms());
}

#[test]
fn a_run_whose_list_never_syncs_still_saves_what_it_counted() {
    // A snapshot over the rule limit leaves the cache unknown for the run.
    let mut hits = restored(usual(true));
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| false);
    hits.record(
        &[ev("b", 1)],
        UPTIME_UP + 1,
        HITS_THEN + 3,
        FIRST + 1_000,
        |_| false,
    );
    no_new_gap(&hits);
    let out = hits.to_saved().unwrap();
    let mut got: Vec<_> = out
        .hits
        .iter()
        .map(|h| (h.name.as_str(), h.count))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![("a", 6), ("b", 1)],
        "restored and waiting, merged"
    );
    assert_eq!(
        out.daemon,
        Some(baseline(FIRST + 1_000, UPTIME_UP + 1, HITS_THEN + 3))
    );
}

#[test]
fn a_waiting_hit_on_a_counted_rule_is_saved_with_its_count() {
    // After a daemon reconnect (cache unknown) a counted rule takes new hits
    // on the side until the next snapshot.
    let mut hits = RuleHits::default();
    hits.trust_daemon_counters(true);
    hits.record(&[ev("a", 1)], 50, 1, NOW, |_| true);
    hits.record(&[ev("a", 2)], 51, 2, NOW + 1_000, |_| false);
    let out = hits.to_saved().unwrap();
    assert_eq!(out.hits.len(), 1);
    assert_eq!((out.hits[0].count, out.hits[0].last_hit_unix_ms), (2, 0));
    assert_eq!(out.daemon, Some(baseline(NOW + 1_000, 51, 2)));
}

#[test]
fn counts_cut_off_at_the_entry_limit_save_no_baseline() {
    // Not every event counted since the baseline is in the file then.
    let many: Vec<(String, u64)> = (0..MAX_TRACKED_RULES)
        .map(|i| (format!("r{i:05}"), 1))
        .collect();
    let refs: Vec<(&str, u64)> = many.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    let mut hits = restored(Saved {
        hits: saved(&refs).hits,
        ..usual(true)
    });
    assert!(
        hits.to_saved().unwrap().daemon.is_some(),
        "exactly at the limit"
    );
    hits.record(&[ev("waiting", 1)], UPTIME_UP, HITS_THEN + 1, FIRST, |_| {
        false
    });
    let out = hits.to_saved().unwrap();
    assert_eq!(out.hits.len(), MAX_TRACKED_RULES);
    assert_eq!(out.daemon, None);
}

#[test]
fn over_tcp_the_shutdown_save_marks_no_clean_stop() {
    // An untrusting bridge writes the version 1 shape.
    let mut hits = RuleHits::default();
    hits.record(&a_events(1), 50, 7, NOW, |_| true);
    let out = hits.to_saved_at_stop(NOW + 1).unwrap();
    assert_eq!((out.daemon, out.stopped_unix_ms), (None, None));
}

#[test]
fn a_restore_that_cannot_be_trusted_after_all_is_a_gap_now() {
    // The restored clean stop couldn't be consumed (`RuleHitsHandle`).
    let mut hits = restored(usual(true));
    hits.distrust_restart(NOW + 1_000);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 1_000));
    assert_eq!(hits.to_saved().unwrap().last_gap_unix_ms, Some(NOW + 1_000));
    assert_eq!(
        hits.to_saved().unwrap().daemon,
        None,
        "nothing to judge from"
    );
    hits.record(&a_events(2), UPTIME_UP, HITS_THEN + 2, FIRST, |_| true);
    assert_eq!(
        hits.last_gap_unix_ms(),
        Some(NOW + 1_000),
        "only a baseline now"
    );
    assert_eq!(
        hits.to_saved().unwrap().daemon,
        Some(baseline(FIRST, UPTIME_UP, HITS_THEN + 2))
    );
}

// #120 item 27 / #159: a clean stop is believed only when the restarted
// daemon started at or after it and within `CLEAN_STOP_WINDOW_MS`.

/// The stop `usual(true)` carries.
const STOPPED: i64 = LAST_PING + 1_000;

/// The restarted daemon started at `start` (ms, a whole second) and its first
/// ping holds every hit, arriving at `first`.
fn restart_started_at(start: i64, first: i64) -> RuleHits {
    let mut hits = restored(usual(true));
    let uptime = ((first - start) / 1_000) as u64;
    hits.record(&a_events(3), uptime, 3, first, |_| true);
    hits
}

fn gap_at(hits: &RuleHits, at: i64) {
    assert_eq!(hits.last_gap_unix_ms(), Some(at));
}

#[test]
fn a_quick_restart_after_the_stop_is_no_gap() {
    // A reboot: the daemon starts a minute after the bridge stopped.
    let hits = restart_started_at(STOPPED + 60_000, FIRST);
    no_new_gap(&hits);
}

#[test]
fn a_restart_after_a_long_power_off_is_a_gap() {
    // Off for a day: the first ping holds every hit of the new run, but a
    // stop this old can't vouch for it.
    let first = STOPPED + DAY + 30_000;
    let hits = restart_started_at(first - 20_000, first);
    gap_at(&hits, first);
}

#[test]
fn a_daemon_that_started_before_the_stop_is_a_gap() {
    // The daemon restarted while the bridge was still running (or the clock
    // stepped back): the stop says nothing about its start.
    let hits = restart_started_at(STOPPED - 5_000, FIRST);
    gap_at_first_ping(&hits);
}

#[test]
fn the_window_is_inclusive_at_its_far_edge_and_the_stop_at_its_near_one() {
    let at_stop = restart_started_at(STOPPED, FIRST);
    no_new_gap(&at_stop);
    let first = STOPPED + CLEAN_STOP_WINDOW_MS + 30_000;
    let last_second = restart_started_at(STOPPED + CLEAN_STOP_WINDOW_MS, first);
    no_new_gap(&last_second);
    let past = restart_started_at(STOPPED + CLEAN_STOP_WINDOW_MS + 1_000, first);
    gap_at(&past, first);
    let before = restart_started_at(STOPPED - 1_000, FIRST);
    gap_at_first_ping(&before);
}

#[test]
fn a_missing_uptime_cannot_place_the_start_and_is_a_gap() {
    // `Statistics.uptime` absent decodes to 0: the start would read as `now`,
    // inside any window.
    let mut hits = restored(usual(true));
    hits.record(&a_events(3), 0, 3, FIRST, |_| true);
    gap_at_first_ping(&hits);
}

#[test]
fn a_clock_that_stepped_back_past_the_stop_is_a_gap() {
    // The first ping's time is before the stop: the start estimate is too.
    let first = STOPPED - 30_000;
    let hits = restart_started_at(first - 10_000, first);
    gap_at(&hits, first);
}

#[test]
fn a_stale_unix_stop_that_outlives_a_tcp_run_is_a_gap_outside_the_window() {
    // #159: Unix run A stops (STOPPED); TCP run T keeps the file and counts
    // hits it never saves; the daemon restarts an hour later and Unix run B
    // reads A's stop. Its first ping holds every hit of the new run, which
    // used to read as "no gap".
    let first = STOPPED + 3_600_000 + 20_000;
    let hits = restart_started_at(first - 20_000, first);
    gap_at(&hits, first);
}

// Review of PR #162: the daemon pings right after it connects, and that ping
// can carry `uptime` 0. The judgement waits for one that doesn't.

/// A ping of a restarted daemon: `n` hits, the counter at `rule_hits`.
fn ping(hits: &mut RuleHits, n: usize, uptime: u64, rule_hits: u64, at: i64) {
    hits.record(&a_events(n), uptime, rule_hits, at, |_| true);
}

#[test]
fn a_quick_restart_whose_first_ping_has_no_uptime_is_judged_at_the_next() {
    let mut hits = restored(usual(true));
    ping(&mut hits, 2, 0, 2, FIRST);
    assert!(hits.is_lossy(), "undecided: still provisional");
    // The daemon started at `FIRST`, after the stop and inside the window;
    // the two pings together hold every hit of its run.
    ping(&mut hits, 1, 1, 3, FIRST + 1_000);
    no_new_gap(&hits);
}

#[test]
fn a_restart_judged_late_still_needs_every_hit_to_have_arrived() {
    let mut hits = restored(usual(true));
    ping(&mut hits, 2, 0, 2, FIRST);
    ping(&mut hits, 1, 1, 5, FIRST + 1_000);
    assert_eq!(hits.last_gap_unix_ms(), Some(FIRST + 1_000));
}

#[test]
fn a_counter_that_fell_between_the_deferred_pings_is_a_gap() {
    // The daemon restarted again before it reported an uptime.
    let mut hits = restored(usual(true));
    // 1 + 1 events and `rule_hits` 2 would agree, but it was 3 a ping ago.
    ping(&mut hits, 1, 0, 3, FIRST);
    ping(&mut hits, 1, 1, 2, FIRST + 1_000);
    assert_eq!(hits.last_gap_unix_ms(), Some(FIRST + 1_000));
}

#[test]
fn an_uptime_that_never_comes_is_a_gap_in_the_file_and_vouches_for_nothing() {
    let mut hits = restored(usual(true));
    ping(&mut hits, 2, 0, 2, FIRST);
    ping(&mut hits, 1, 0, 3, FIRST + 1_000);
    assert_eq!(hits.last_gap_unix_ms(), Some(FIRST + 1_000));
    for saved in [
        hits.to_saved().unwrap(),
        hits.to_saved_at_stop(FIRST + 2_000).unwrap(),
    ] {
        assert_eq!(saved.last_gap_unix_ms, Some(FIRST + 1_000), "saved");
        assert_eq!((saved.daemon, saved.stopped_unix_ms), (None, None));
    }
}

#[test]
fn a_restore_with_no_ping_at_all_still_hands_the_old_baseline_on() {
    // Unchanged by the deferral: nothing was counted yet.
    let hits = restored(usual(true));
    assert!(hits.to_saved().unwrap().daemon.is_some());
}

#[test]
fn a_daemon_that_restarted_after_the_deferred_ping_is_a_gap() {
    // Run X pings with uptime 0 (2 hits, counter 2) and dies. Run Y starts a
    // minute later and loses 2 hits before it pings (1 hit, counter 3). Both
    // `rule_hits = received` (3 = 2 + 1) and the within-run check (2 -> 3
    // grew by the 1 received) pass: only Y's start, after X's ping, shows it.
    let mut hits = restored(usual(true));
    ping(&mut hits, 2, 0, 2, FIRST);
    let second = FIRST + 90_000;
    ping(&mut hits, 1, 30, 3, second);
    assert_eq!(hits.last_gap_unix_ms(), Some(second));
}
