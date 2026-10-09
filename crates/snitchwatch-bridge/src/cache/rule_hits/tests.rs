use super::*;
use snitchwatch_proto::protocol::{Event, Rule};

const NOW: i64 = 1_800_000_000_000;

fn ev(rule: &str, unixnano: i64) -> Event {
    Event {
        rule: Some(Rule {
            name: rule.to_string(),
            ..Default::default()
        }),
        unixnano,
        ..Default::default()
    }
}

fn ruleless() -> Event {
    Event {
        unixnano: 1,
        ..Default::default()
    }
}

/// Records `events` as a ping from a daemon that lost nothing: uptime 100,
/// and its `rule_hits` counter grown by exactly the events it sends.
fn ping(hits: &mut RuleHits, events: &[Event], known: impl Fn(&str) -> bool) -> bool {
    let rule_hits = hits.last_rule_hits.unwrap_or(0) + events.len() as u64;
    hits.record(events, 100, rule_hits, NOW, known)
}

/// [`ping`] where `names` are the rules the cache knows.
fn rec(hits: &mut RuleHits, events: &[Event], names: &[&str]) -> bool {
    ping(hits, events, |n| names.contains(&n))
}

fn counts(hits: &RuleHits) -> Vec<(String, u64)> {
    hits.wire_hits()
        .into_iter()
        .map(|h| (h.name, h.count))
        .collect()
}

fn pair(name: &str, count: u64) -> (String, u64) {
    (name.to_string(), count)
}

#[test]
fn counts_each_rule_and_takes_the_last_hit_from_unixnano() {
    let mut hits = RuleHits::default();
    rec(
        &mut hits,
        &[
            ev("a", 1_700_000_000_000_000_000),
            ev("a", 1_700_000_005_000_000_000),
            ev("b", 1_700_000_001_000_000_000),
            ev("a", 1_700_000_003_000_000_000),
        ],
        &["a", "b"],
    );
    assert_eq!(counts(&hits), vec![pair("a", 3), pair("b", 1)]);
    let wire = hits.wire_hits();
    assert_eq!(
        wire[0].last_hit_unix_ms, 1_700_000_005_000,
        "latest, not last"
    );
    assert_eq!(wire[1].last_hit_unix_ms, 1_700_000_001_000);
}

#[test]
fn an_event_without_a_rule_is_ignored() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ruleless(), ev("", 5)], &["a"]);
    assert!(counts(&hits).is_empty());
}

#[test]
fn a_missing_timestamp_falls_back_to_now() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 0)], &["a"]);
    assert_eq!(hits.wire_hits()[0].last_hit_unix_ms, NOW);
}

#[test]
fn a_name_the_cache_does_not_know_never_reaches_the_wire_and_is_dropped_at_commit() {
    // The bridge's once-reply rules are named by the bridge and are not in
    // the daemon's list.
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("snitchwatch-once-7", 1)], &["a"]);
    assert!(counts(&hits).is_empty());
    hits.adopt_snapshot(NOW, |n| n == "a");
    assert!(counts(&hits).is_empty());
    // Dropped for good: a later snapshot that does name it adopts nothing.
    hits.adopt_snapshot(NOW, |_| true);
    assert!(counts(&hits).is_empty());
}

#[test]
fn counts_made_before_the_cache_is_synced_are_adopted_at_the_commit_that_has_the_name() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1), ev("a", 2), ev("b", 3)], &[]);
    assert!(counts(&hits).is_empty(), "held back until a snapshot");
    hits.adopt_snapshot(NOW, |n| n == "a");
    assert_eq!(
        counts(&hits),
        vec![pair("a", 2)],
        "b is not in the snapshot"
    );
}

#[test]
fn counts_survive_a_reconnect_and_hits_in_the_gap_are_added_on_adoption() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1), ev("a", 2), ev("a", 3)], &["a"]);
    // The stream closes: `RulesSync::withdraw` never reaches `RuleHits`.
    // The cache is Unknown meanwhile, so two more hits wait on the side.
    rec(&mut hits, &[ev("a", 4), ev("a", 5)], &[]);
    assert_eq!(counts(&hits), vec![pair("a", 3)]);
    hits.adopt_snapshot(NOW, |n| n == "a");
    assert_eq!(counts(&hits), vec![pair("a", 5)]);
}

#[test]
fn a_new_snapshot_without_a_rule_drops_its_count() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1), ev("b", 2)], &["a", "b"]);
    hits.adopt_snapshot(NOW, |n| n == "a");
    assert_eq!(counts(&hits), vec![pair("a", 1)]);
}

/// A rule that leaves a committed snapshot loses its count. If it returns
/// (a file put back, with its old `created`), it would start again at 0 with
/// nothing to say that its earlier hits are gone, and the Rules page could
/// call it unused at once. So the loss is a gap: nothing before it is trusted.
#[test]
fn a_rule_that_leaves_a_snapshot_loses_its_count_and_that_is_a_gap() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1), ev("b", 2)], &["a", "b"]);
    assert_eq!(hits.last_gap_unix_ms(), None);
    hits.adopt_snapshot(NOW + 5, |n| n == "a");
    assert_eq!(counts(&hits), vec![pair("a", 1)]);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 5));
}

#[test]
fn a_snapshot_that_keeps_every_counted_rule_is_no_gap() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1)], &["a"]);
    hits.adopt_snapshot(NOW + 5, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), None);
    // A name that was only waiting (never counted) isn't lost history.
    rec(&mut hits, &[ev("once", 2)], &[]);
    hits.adopt_snapshot(NOW + 6, |n| n == "a");
    assert_eq!(hits.last_gap_unix_ms(), None);
}

#[test]
fn forgetting_a_rule_drops_its_count_wherever_it_is() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1), ev("b", 2)], &["a"]);
    hits.forget("a");
    hits.forget("b");
    assert!(counts(&hits).is_empty());
    hits.adopt_snapshot(NOW, |_| true);
    assert!(counts(&hits).is_empty(), "the side entry for b went too");
}

#[test]
fn a_rule_the_cache_learns_later_takes_its_waiting_counts_with_it() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a", 1)], &[]);
    rec(&mut hits, &[ev("a", 2)], &["a"]);
    assert_eq!(counts(&hits), vec![pair("a", 2)]);
}

/// A ping at `now` from a daemon whose `rule_hits` counter reads
/// `rule_hits`, every rule known.
fn at(hits: &mut RuleHits, events: &[Event], rule_hits: u64, now: i64) {
    hits.record(events, 100, rule_hits, now, |_| true);
}

#[test]
fn the_first_ping_only_sets_the_baseline() {
    // The daemon's counter also covers the time before counting began.
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 10_000, NOW);
    assert!(!hits.is_lossy());
    assert_eq!(counts(&hits), vec![pair("a", 1)]);
}

#[test]
fn a_counter_that_grew_by_exactly_the_events_is_complete() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 7, NOW);
    at(&mut hits, &[ev("a", 2), ev("b", 3)], 9, NOW + 1_000);
    at(&mut hits, &[], 9, NOW + 2_000);
    assert!(!hits.is_lossy());
}

#[test]
fn a_counter_that_grew_by_more_than_the_events_is_a_gap() {
    // Dropped at the daemon's cap, or appended between `Serialize`'s unlock
    // and `emptyStats`: counted by `RuleHits`, never sent.
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 10, NOW);
    at(&mut hits, &[ev("a", 2), ev("a", 3)], 13, NOW + 5);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 5));
    assert_eq!(counts(&hits), vec![pair("a", 3)], "what did arrive counts");
}

#[test]
fn a_failed_ping_loses_its_batch_and_the_next_ping_shows_it() {
    // `client.go` `ping`: `Serialize` empties the batch before the RPC, and
    // a failed RPC is never resent.
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1), ev("a", 2), ev("a", 3)], 3, NOW);
    // The daemon's next ping (events 4-6, counter 6) fails.
    at(&mut hits, &[ev("a", 7)], 7, NOW + 2_000);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 2_000));
    assert_eq!(counts(&hits), vec![pair("a", 4)]);
}

#[test]
fn the_raw_length_counts_not_the_events_with_a_rule() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 1, NOW);
    at(&mut hits, &[ruleless(), ev("a", 2)], 3, NOW + 1);
    assert!(!hits.is_lossy());
}

#[test]
fn the_baseline_moves_with_every_ping() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 1, NOW - 1_000);
    at(&mut hits, &[ev("a", 2)], 5, NOW);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
    at(&mut hits, &[ev("a", 3), ev("a", 4)], 7, NOW + 5_000);
    assert_eq!(
        hits.last_gap_unix_ms(),
        Some(NOW),
        "measured from the last ping, not the first"
    );
}

#[test]
fn a_counter_that_went_down_is_a_restart_gap() {
    // A restart that `uptime` misses: the daemon was idle (no ping) for
    // longer than it had run before.
    let mut hits = RuleHits::default();
    hits.record(&[ev("a", 1)], 1_000, 100, NOW, |_| true);
    hits.record(&[ev("a", 2), ev("a", 3)], 5_000, 3, NOW + 9, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 9));
    hits.record(&[ev("a", 4)], 5_001, 4, NOW + 20, |_| true);
    assert_eq!(
        hits.last_gap_unix_ms(),
        Some(NOW + 9),
        "the new run is the baseline"
    );
    assert_eq!(counts(&hits), vec![pair("a", 4)], "nothing zeroed");
}

#[test]
fn more_events_than_the_counter_grew_is_a_restart_gap() {
    // Impossible within one daemon run (every event adds one), so the
    // daemon restarted and its new counter passed the old one.
    let mut hits = RuleHits::default();
    hits.record(&[ev("a", 1)], 1_000, 100, NOW, |_| true);
    let batch: Vec<Event> = (0..60).map(|i| ev("a", i + 2)).collect();
    hits.record(&batch, 5_000, 150, NOW + 9, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 9));
}

#[test]
fn incomplete_counts_stay_incomplete() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 1, NOW);
    at(&mut hits, &[ev("a", 2)], 9, NOW);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
    rec(&mut hits, &[ev("a", 3)], &["a"]);
    assert!(hits.is_lossy());
}

#[test]
fn a_daemon_restart_is_a_gap_and_does_not_zero_the_counts() {
    let mut hits = RuleHits::default();
    hits.record(&[ev("a", 1)], 500, 1, NOW, |_| true);
    hits.record(&[ev("a", 2)], 501, 2, NOW, |_| true);
    hits.record(&[ev("a", 3)], 501, 3, NOW, |_| true);
    assert!(!hits.is_lossy(), "uptime grew or stood still");
    // The new run's counter grew by exactly its events since the old
    // one's last ping: only `uptime` says.
    hits.record(&[ev("a", 4)], 3, 4, NOW + 7, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 7), "uptime dropped");
    assert_eq!(counts(&hits), vec![pair("a", 4)]);
}

#[test]
fn counting_starts_at_the_first_ping_with_statistics_and_stays_there() {
    let mut hits = RuleHits::default();
    assert_eq!(hits.since_unix_ms(), None);
    hits.record(&[], 1, 0, NOW, |_| true);
    assert_eq!(hits.since_unix_ms(), Some(NOW));
    hits.record(&[ev("a", 1)], 2, 1, NOW + 9_000, |_| true);
    assert_eq!(hits.since_unix_ms(), Some(NOW));
}

#[test]
fn only_a_change_bumps_the_revision() {
    let mut hits = RuleHits::default();
    assert!(rec(&mut hits, &[ev("a", 1)], &["a"]));
    let rev = hits.revision();
    assert!(!rec(&mut hits, &[ruleless()], &["a"]), "nothing to count");
    assert_eq!(hits.revision(), rev);
    assert!(rec(&mut hits, &[ev("a", 2)], &["a"]));
    assert!(hits.revision() > rev);
}

#[test]
fn the_side_map_is_bounded_and_overflow_is_a_gap() {
    let mut hits = RuleHits::default();
    let names: Vec<String> = (0..=SIDE_MAP_MAX).map(|i| format!("n{i}")).collect();
    let events: Vec<Event> = names.iter().map(|n| ev(n, 1)).collect();
    for chunk in events.chunks(50) {
        ping(&mut hits, chunk, |_| false);
    }
    assert!(
        hits.is_lossy(),
        "the {}th name had nowhere to go",
        SIDE_MAP_MAX + 1
    );
    hits.adopt_snapshot(NOW, |_| true);
    assert_eq!(hits.wire_hits().len(), SIDE_MAP_MAX);
    // A name already waiting still counts when the map is full.
    let mut hits = RuleHits::default();
    for chunk in events[..SIDE_MAP_MAX].chunks(50) {
        ping(&mut hits, chunk, |_| false);
    }
    ping(&mut hits, &[ev("n0", 2)], |_| false);
    assert!(!hits.is_lossy());
    hits.adopt_snapshot(NOW, |n| n == "n0");
    assert_eq!(counts(&hits), vec![pair("n0", 2)]);
}

#[test]
fn the_main_map_is_bounded_and_overflow_is_a_gap() {
    let mut hits = RuleHits::default();
    let names: Vec<String> = (0..=MAX_TRACKED_RULES)
        .map(|i| format!("r{i:05}"))
        .collect();
    for chunk in names.chunks(100) {
        let events: Vec<Event> = chunk.iter().map(|n| ev(n, 1)).collect();
        ping(&mut hits, &events, |_| true);
    }
    assert_eq!(hits.wire_hits().len(), MAX_TRACKED_RULES);
    assert!(hits.is_lossy());
}

#[test]
fn a_name_too_long_to_keep_is_a_gap_not_a_count() {
    let mut hits = RuleHits::default();
    let long = "x".repeat(MAX_HIT_NAME_BYTES + 1);
    rec(&mut hits, &[ev(&long, 1)], &[&long]);
    assert!(counts(&hits).is_empty());
    assert!(hits.is_lossy());
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev("a\u{1}b", 1)], &["a\u{1}b"]);
    assert!(counts(&hits).is_empty(), "control characters");
    assert!(hits.is_lossy());
    let fits = "x".repeat(MAX_HIT_NAME_BYTES);
    let mut hits = RuleHits::default();
    rec(&mut hits, &[ev(&fits, 1)], &[&fits]);
    assert_eq!(hits.wire_hits().len(), 1);
}

fn saved(hits: &[(&str, u64)]) -> Saved {
    Saved {
        since_unix_ms: NOW - 86_400_000,
        last_gap_unix_ms: None,
        hits: hits
            .iter()
            .map(|(name, count)| RuleHitWire {
                name: (*name).to_string(),
                count: *count,
                last_hit_unix_ms: NOW - 1_000,
            })
            .collect(),
        daemon: None,
        stopped_unix_ms: None,
    }
}

#[test]
fn a_restored_file_keeps_its_start_time_and_means_the_bridge_was_down() {
    let mut hits = RuleHits::default();
    hits.restore(saved(&[("a", 4)]), NOW);
    assert_eq!(hits.since_unix_ms(), Some(NOW - 86_400_000));
    assert!(
        hits.is_lossy(),
        "no event was counted while the bridge was down"
    );
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
}

#[test]
fn restored_counts_wait_for_the_first_snapshot_and_only_names_in_it_come_back() {
    let mut hits = RuleHits::default();
    hits.restore(saved(&[("a", 4), ("gone", 9)]), NOW);
    assert!(
        counts(&hits).is_empty(),
        "no rule list to check them against yet"
    );
    rec(&mut hits, &[ev("a", 5)], &[]);
    hits.adopt_snapshot(NOW, |n| n == "a");
    assert_eq!(
        counts(&hits),
        vec![pair("a", 5)],
        "4 saved + 1 new; gone pruned"
    );
    // Used up: a later snapshot cannot bring `gone` back.
    hits.adopt_snapshot(NOW, |_| true);
    assert_eq!(counts(&hits), vec![pair("a", 5)]);
}

#[test]
fn counts_not_yet_adopted_are_still_saved() {
    // Five minutes with no daemon must not wipe the history.
    let mut hits = RuleHits::default();
    hits.restore(saved(&[("a", 4), ("b", 1)]), NOW);
    let out = hits.to_saved().expect("counting has started");
    let mut got: Vec<_> = out
        .hits
        .iter()
        .map(|h| (h.name.as_str(), h.count))
        .collect();
    got.sort();
    assert_eq!(got, vec![("a", 4), ("b", 1)]);
    assert_eq!(out.since_unix_ms, NOW - 86_400_000);
}

#[test]
fn saving_leaves_out_the_side_map_and_nothing_counted_yet() {
    let mut hits = RuleHits::default();
    hits.restore(saved(&[("a", 4)]), NOW);
    rec(&mut hits, &[ev("a", 5), ev("once-1", 6)], &[]);
    let out = hits.to_saved().unwrap();
    assert_eq!(out.hits.len(), 1, "waiting counts are not confirmed yet");
    assert_eq!(out.hits[0].count, 4);

    let mut live = RuleHits::default();
    rec(&mut live, &[ev("a", 1)], &["a"]);
    assert_eq!(live.to_saved().unwrap().hits[0].count, 1);
    assert!(
        RuleHits::default().to_saved().is_none(),
        "nothing counted yet"
    );
}

#[test]
fn saved_counts_are_capped_at_the_entry_limit() {
    let mut hits = RuleHits::default();
    let many: Vec<(String, u64)> = (0..MAX_TRACKED_RULES + 5)
        .map(|i| (format!("r{i:05}"), 1))
        .collect();
    let refs: Vec<(&str, u64)> = many.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    hits.restore(saved(&refs), NOW);
    assert_eq!(hits.to_saved().unwrap().hits.len(), MAX_TRACKED_RULES);
}

#[test]
fn live_and_restored_counts_together_stay_within_the_saved_limit() {
    let mut hits = RuleHits::default();
    let old: Vec<(String, u64)> = (0..MAX_TRACKED_RULES)
        .map(|i| (format!("old{i:05}"), 1))
        .collect();
    let refs: Vec<(&str, u64)> = old.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    hits.restore(saved(&refs), NOW);
    let live: Vec<String> = (0..10).map(|i| format!("live{i}")).collect();
    for name in &live {
        rec(&mut hits, &[ev(name, 1)], &[name.as_str()]);
    }
    let out = hits.to_saved().unwrap();
    assert_eq!(out.hits.len(), MAX_TRACKED_RULES, "the file's own limit");
    for name in &live {
        assert!(
            out.hits.iter().any(|h| &h.name == name),
            "{name}: the live counts go first"
        );
    }
}

#[test]
fn a_hit_time_too_far_ahead_is_taken_as_now() {
    // One daemon event with a wild `unixnano` must not make every later
    // save fail the file's own check.
    let mut hits = RuleHits::default();
    let far = (NOW + 2 * MAX_FUTURE_SKEW_MS) * 1_000_000;
    let near = (NOW + MAX_FUTURE_SKEW_MS / 2) * 1_000_000;
    rec(&mut hits, &[ev("a", far), ev("b", near)], &["a", "b"]);
    let wire = hits.wire_hits();
    assert_eq!(wire[0].last_hit_unix_ms, NOW, "far ahead");
    assert_eq!(wire[1].last_hit_unix_ms, NOW + MAX_FUTURE_SKEW_MS / 2);
}

// E3 (plan `2026-10-08-default-applied-events.md`): the bazzite-tower fork
// reports each connection that got the daemon's `DefaultAction` as an event
// whose synthetic rule is named "" and described by the marker. It grows
// `rule_misses`, never `rule_hits`.

fn marked(unixnano: i64) -> Event {
    Event {
        rule: Some(Rule {
            name: String::new(),
            description: crate::daemon_contract::DEFAULT_ACTION_MARKER.to_string(),
            action: "deny".to_string(),
            duration: "once".to_string(),
            enabled: true,
            ..Default::default()
        }),
        unixnano,
        ..Default::default()
    }
}

#[test]
fn default_applied_events_are_not_lost_rule_hits() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1), marked(2)], 10, NOW);
    // Only marked events: the counter stays.
    at(&mut hits, &[marked(3), marked(4)], 10, NOW + 1_000);
    // A mix: the counter grows by the rule events only.
    at(
        &mut hits,
        &[marked(5), ev("a", 6), marked(7), ev("b", 8)],
        12,
        NOW + 2_000,
    );
    assert!(!hits.is_lossy(), "gap at {:?}", hits.last_gap_unix_ms());
    assert_eq!(counts(&hits), vec![pair("a", 2), pair("b", 1)]);
}

#[test]
fn a_real_gap_is_still_found_among_default_applied_events() {
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 10, NOW);
    // Three rule hits happened; one arrived, beside two marked events.
    at(&mut hits, &[marked(2), ev("a", 3), marked(4)], 13, NOW + 5);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 5));
}

#[test]
fn a_default_applied_event_counts_for_no_rule() {
    let mut hits = RuleHits::default();
    // Even with every name known, "" included.
    at(&mut hits, &[marked(1), marked(2)], 0, NOW);
    assert!(counts(&hits).is_empty());
    hits.adopt_snapshot(NOW, |_| true);
    assert!(counts(&hits).is_empty(), "nothing waits on the side either");
}

#[test]
fn a_named_rule_with_the_marker_description_is_an_ordinary_hit() {
    let mut named = marked(2);
    named.rule.as_mut().unwrap().name = "copied".to_string();
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 10, NOW);
    // It grew `rule_hits`, so it is received, and it counts.
    at(&mut hits, &[named], 11, NOW + 1_000);
    assert!(!hits.is_lossy(), "gap at {:?}", hits.last_gap_unix_ms());
    assert_eq!(counts(&hits), vec![pair("a", 1), pair("copied", 1)]);
}

#[test]
fn an_unmarked_event_named_empty_is_a_rule_hit_received() {
    // Stock v1.8.0 can load a hand-written rule named "": it grows
    // `rule_hits` and is counted in the arithmetic, though not by name.
    let mut hits = RuleHits::default();
    at(&mut hits, &[ev("a", 1)], 10, NOW);
    at(&mut hits, &[ev("", 2), ev("a", 3)], 12, NOW + 1_000);
    assert!(!hits.is_lossy(), "gap at {:?}", hits.last_gap_unix_ms());
    assert_eq!(counts(&hits), vec![pair("a", 2)]);
}

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
