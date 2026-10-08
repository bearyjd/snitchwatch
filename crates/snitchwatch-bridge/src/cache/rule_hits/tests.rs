use super::*;
use snitchwatch_proto::protocol::{Event, Rule};

const NOW: i64 = 1_800_000_000_000;
const MAX: usize = 150;

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

/// Records `events` as a ping with uptime 100 where `names` are the rules
/// the cache knows.
fn rec(hits: &mut RuleHits, events: &[Event], names: &[&str]) -> bool {
    hits.record(events, 100, MAX, NOW, |n| names.contains(&n))
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

fn batch(n: usize) -> Vec<Event> {
    (0..n).map(|_| ev("a", 1)).collect()
}

#[test]
fn a_batch_near_the_daemons_cap_marks_the_counts_incomplete() {
    // `max_events - 1`: a missed connection at the cap drops an event first.
    for (len, lossy) in [(MAX, true), (MAX - 1, true), (MAX - 2, false), (0, false)] {
        let mut hits = RuleHits::default();
        rec(&mut hits, &batch(len), &["a"]);
        assert_eq!(hits.is_lossy(), lossy, "batch of {len}");
    }
}

#[test]
fn the_cap_is_the_daemons_own_and_counts_events_without_a_rule() {
    let mut hits = RuleHits::default();
    hits.record(&batch(8), 100, 10, NOW, |_| true);
    assert!(!hits.is_lossy());
    hits.record(&batch(9), 100, 10, NOW, |_| true);
    assert!(hits.is_lossy(), "9 of 10");

    let mut hits = RuleHits::default();
    let mut events = vec![ruleless(); 9];
    events.push(ev("a", 1));
    hits.record(&events, 100, 10, NOW, |_| true);
    assert!(
        hits.is_lossy(),
        "the raw length counts, not the filtered one"
    );
}

#[test]
fn incomplete_counts_stay_incomplete() {
    let mut hits = RuleHits::default();
    rec(&mut hits, &batch(MAX), &["a"]);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW));
    rec(&mut hits, &batch(1), &["a"]);
    assert!(hits.is_lossy());
}

#[test]
fn a_daemon_restart_is_a_gap_and_does_not_zero_the_counts() {
    let mut hits = RuleHits::default();
    hits.record(&[ev("a", 1)], 500, MAX, NOW, |_| true);
    hits.record(&[ev("a", 2)], 501, MAX, NOW, |_| true);
    hits.record(&[ev("a", 3)], 501, MAX, NOW, |_| true);
    assert!(!hits.is_lossy(), "uptime grew or stood still");
    hits.record(&[ev("a", 4)], 3, MAX, NOW + 7, |_| true);
    assert_eq!(hits.last_gap_unix_ms(), Some(NOW + 7), "uptime dropped");
    assert_eq!(counts(&hits), vec![pair("a", 4)]);
}

#[test]
fn counting_starts_at_the_first_ping_with_statistics_and_stays_there() {
    let mut hits = RuleHits::default();
    assert_eq!(hits.since_unix_ms(), None);
    hits.record(&[], 1, MAX, NOW, |_| true);
    assert_eq!(hits.since_unix_ms(), Some(NOW));
    hits.record(&[ev("a", 1)], 2, MAX, NOW + 9_000, |_| true);
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
    // One ping at a time: the cap would otherwise be the daemon's own.
    for chunk in events.chunks(50) {
        hits.record(chunk, 100, 1_000_000, NOW, |_| false);
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
        hits.record(chunk, 100, 1_000_000, NOW, |_| false);
    }
    hits.record(&[ev("n0", 2)], 100, 1_000_000, NOW, |_| false);
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
        hits.record(&events, 100, 1_000_000, NOW, |_| true);
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
