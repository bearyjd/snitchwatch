//! Tests for [`super`]: when a persistent disagreement between the daemon's
//! rule count and the list becomes a hint, and when it does not.

use super::*;
use crate::cache::rules::{bounded_snapshot, lock, PendingSnapshots, MAX_RULE_FIELD_BYTES};
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Operator, Rule};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

fn rule(name: &str, duration: &str, enabled: bool) -> Rule {
    Rule {
        created: 1_800_000_000,
        name: name.to_string(),
        enabled,
        action: "allow".to_string(),
        duration: duration.to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A list of `n` ordinary (`always`) rules, already read once.
fn synced(n: usize) -> RulesCache {
    let mut cache = RulesCache::default();
    cache.replace_all(
        (0..n)
            .map(|i| rule(&format!("r{i}"), "always", true))
            .collect(),
    );
    cache
}

fn read_at(cache: &mut RulesCache, reported: u64, uptime: u64) -> bool {
    cache.observe_rule_count(Reading {
        reported,
        uptime,
        settling: false,
        now: Instant::now(),
    })
}

fn read(cache: &mut RulesCache, reported: u64) -> bool {
    read_at(cache, reported, 100)
}

/// `reported` readings in a row; whether any flipped the hint.
fn read_times(cache: &mut RulesCache, reported: u64, times: usize) -> Vec<bool> {
    (0..times).map(|_| read(cache, reported)).collect()
}

/// The quiet readings after a change, spent on agreeing counts.
fn settle(cache: &mut RulesCache) {
    let agreeing = cache
        .rules()
        .map_or(0, |r| r.len() + cache.left_out().len()) as u64;
    for _ in 0..QUIET_PINGS {
        assert!(!read(cache, agreeing));
    }
}

fn raised(cache: &mut RulesCache, daemon_has: u64) {
    settle(cache);
    let flips = read_times(cache, daemon_has, usize::from(PINGS_TO_RAISE));
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
    assert!(cache.count_mismatch());
}

#[test]
fn agreeing_counts_never_raise() {
    let mut cache = synced(2);
    assert!(read_times(&mut cache, 2, 50).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());
}

#[test]
fn a_disagreement_has_to_repeat_to_raise_the_hint() {
    let mut cache = synced(2);
    settle(&mut cache);
    let flips = read_times(&mut cache, 3, usize::from(PINGS_TO_RAISE));
    assert_eq!(flips, vec![false, false, true]);
    assert!(cache.count_mismatch());
    // Nothing flips again while it holds.
    assert!(read_times(&mut cache, 3, 20).iter().all(|flip| !flip));
    assert!(cache.count_mismatch());
}

#[test]
fn a_count_that_keeps_moving_never_raises() {
    let mut cache = synced(2);
    settle(&mut cache);
    for reported in 3..40 {
        assert!(!read(&mut cache, reported), "{reported}");
    }
    for reported in [3, 4].into_iter().cycle().take(20) {
        assert!(!read(&mut cache, reported));
    }
    assert!(!cache.count_mismatch());
}

#[test]
fn flapping_never_flips_either_way() {
    let mut cache = synced(2);
    settle(&mut cache);
    for _ in 0..10 {
        assert!(!read(&mut cache, 3));
        assert!(!read(&mut cache, 3));
        assert!(!read(&mut cache, 2));
    }
    assert!(!cache.count_mismatch());

    raised(&mut cache, 3);
    for _ in 0..10 {
        assert!(!read(&mut cache, 2));
        assert!(!read(&mut cache, 2));
        assert!(!read(&mut cache, 3));
    }
    assert!(
        cache.count_mismatch(),
        "two agreeing readings are not enough"
    );
}

#[test]
fn the_hint_clears_only_after_enough_agreeing_readings_and_once() {
    let mut cache = synced(2);
    raised(&mut cache, 3);
    let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_CLEAR));
    assert_eq!(flips, vec![false, false, true]);
    assert!(!cache.count_mismatch());
    assert!(read_times(&mut cache, 2, 20).iter().all(|flip| !flip));
}

/// A reading built before the daemon applied (or the bridge learned of) the
/// bridge's own change is old, and counts for nothing.
#[test]
fn readings_right_after_the_bridges_own_change_are_ignored() {
    let mut cache = synced(2);
    settle(&mut cache);
    read_times(&mut cache, 2, 5);
    // A remembered verdict, a confirmed command, a prune: any list change.
    cache.upsert(rule("new", "always", true));
    // The daemon's readings still say 2 for a moment.
    let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_RAISE));
    assert!(flips.iter().all(|flip| !flip), "{flips:?}");
    assert!(!cache.count_mismatch());
    // Then it says 3, which agrees.
    assert!(read_times(&mut cache, 3, 20).iter().all(|flip| !flip));
}

/// The plan's numbers, literally: two readings are ignored after an own
/// change, and three more repeats of a disagreement raise the hint.
#[test]
fn two_readings_are_ignored_after_an_own_change_and_three_more_raise() {
    let mut cache = synced(2);
    settle(&mut cache);
    cache.upsert(rule("new", "always", true));
    // The daemon never learns of it (say): 2 against a list of 3.
    let flips = read_times(&mut cache, 2, 4);
    assert_eq!(flips, vec![false; 4], "2 ignored, then 2 of 3");
    assert!(read(&mut cache, 2), "the fifth completes the run");
}

#[test]
fn a_run_that_a_change_interrupts_starts_over() {
    let mut cache = synced(2);
    settle(&mut cache);
    assert_eq!(read_times(&mut cache, 3, 2), vec![false, false]);
    cache.upsert(rule("new", "always", true));
    // 4 now disagrees with 3, but the change reset the run and the quiet.
    assert!(read_times(&mut cache, 4, usize::from(QUIET_PINGS) + 2)
        .iter()
        .all(|flip| !flip));
    assert!(!cache.count_mismatch());
}

#[test]
fn an_adopted_snapshot_and_a_withdrawal_clear_the_hint_at_once() {
    let mut cache = synced(2);
    raised(&mut cache, 3);
    cache.replace_all(vec![rule("a", "always", true)]);
    assert!(!cache.count_mismatch());

    let mut cache = synced(2);
    raised(&mut cache, 3);
    cache.set_unknown();
    assert!(!cache.count_mismatch());
    // With no list there is nothing to compare, whatever the daemon says.
    assert!(read_times(&mut cache, 7, 20).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());
}

#[test]
fn a_new_snapshot_is_a_new_baseline() {
    let mut cache = synced(2);
    raised(&mut cache, 3);
    cache.replace_all(
        (0..3)
            .map(|i| rule(&format!("s{i}"), "always", true))
            .collect(),
    );
    // The daemon's reading from before the snapshot, 2, is ignored; then 3.
    assert!(read_times(&mut cache, 2, usize::from(QUIET_PINGS))
        .iter()
        .all(|flip| !flip));
    assert!(read_times(&mut cache, 3, 20).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());
}

#[test]
fn a_restarted_daemon_is_a_quiet_moment() {
    let mut cache = synced(2);
    settle(&mut cache);
    read_at(&mut cache, 3, 500);
    read_at(&mut cache, 3, 501);
    // Uptime fell: another process. Its first readings are not evidence.
    let flips: Vec<bool> = (0..usize::from(QUIET_PINGS) + 1)
        .map(|i| read_at(&mut cache, 3, 3 + i as u64))
        .collect();
    assert!(flips.iter().all(|flip| !flip), "{flips:?}");
    assert!(!cache.count_mismatch());
    // And what came before is forgotten: it takes the quiet readings and
    // the full run again.
    let mut cache = synced(2);
    settle(&mut cache);
    read_at(&mut cache, 3, 500);
    read_at(&mut cache, 3, 501);
    let flips: Vec<bool> = (0..usize::from(QUIET_PINGS + PINGS_TO_RAISE))
        .map(|i| read_at(&mut cache, 3, 3 + i as u64))
        .collect();
    assert_eq!(flips.iter().filter(|flip| **flip).count(), 1, "{flips:?}");
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn rules_left_out_for_size_count_and_deleted_rules_with_files_left_do_not() {
    let mut cache = synced(2);
    cache.set_left_out([("long".to_string(), 20_000)].into());
    settle(&mut cache);
    // 2 listed + 1 left out is what the daemon holds.
    assert!(read_times(&mut cache, 3, 20).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());
    let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_RAISE));
    assert_eq!(flips.last(), Some(&true));

    // A refused delete: the rule left the daemon's memory, its file stayed.
    let mut cache = synced(2);
    cache.apply_refused(&Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![rule("r0", "always", true)],
        ..Default::default()
    });
    assert!(cache.files_left().contains("r0"));
    settle(&mut cache);
    assert!(read_times(&mut cache, 1, 20).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());
}

#[test]
fn a_zero_reading_over_a_list_is_not_evidence() {
    let mut cache = synced(2);
    settle(&mut cache);
    assert!(read_times(&mut cache, 0, 20).iter().all(|flip| !flip));
    assert!(!cache.count_mismatch());

    // An empty list and a zero agree; a daemon with rules does not.
    let mut empty = synced(0);
    settle(&mut empty);
    assert!(read_times(&mut empty, 0, 20).iter().all(|flip| !flip));
    let flips = read_times(&mut empty, 1, usize::from(PINGS_TO_RAISE));
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// The daemon's timer removes a temporary rule whether or not the bridge's
/// approximation has noticed, and a snapshot gives a disabled one no expiry
/// of its own: while one is listed, the count says nothing.
#[test]
fn a_listed_temporary_rule_pauses_the_check_enabled_or_not() {
    for enabled in [true, false] {
        let mut cache = RulesCache::default();
        cache.replace_all(vec![
            rule("a", "always", true),
            rule("timed", "5m", enabled),
        ]);
        settle(&mut cache);
        // The daemon dropped it (its timer fired): one fewer, for good.
        assert!(
            read_times(&mut cache, 1, 30).iter().all(|flip| !flip),
            "enabled: {enabled}"
        );
        assert!(!cache.count_mismatch(), "enabled: {enabled}");
        // Once it is gone from the list the check runs again.
        cache.remove("timed");
        settle(&mut cache);
        assert!(!read_times(&mut cache, 1, 20).iter().any(|flip| *flip));
        let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_RAISE));
        assert_eq!(flips.last(), Some(&true), "enabled: {enabled}");
    }
}

#[test]
fn only_temporary_durations_pause_the_check() {
    for duration in ["always", "until restart", "once"] {
        let mut cache = RulesCache::default();
        cache.replace_all(vec![rule("a", duration, true)]);
        settle(&mut cache);
        let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_RAISE));
        assert_eq!(flips.last(), Some(&true), "{duration}");
    }
    // The daemon parses "1.5h" and "500ms"; the bridge's expiry does not.
    for duration in ["1.5h", "500ms", "5m", ""] {
        let mut cache = RulesCache::default();
        cache.replace_all(vec![rule("a", duration, true)]);
        settle(&mut cache);
        assert!(
            read_times(&mut cache, 2, 20).iter().all(|flip| !flip),
            "{duration:?}"
        );
    }
}

#[test]
fn a_temporary_rule_left_out_for_size_pauses_the_check_too() {
    let mut huge = rule("huge", "5m", true);
    huge.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    let snapshot = bounded_snapshot(vec![rule("a", "always", true), huge]);
    assert!(snapshot.left_out_temporary);
    let mut cache = RulesCache::default();
    cache.replace_all(snapshot.rules);
    cache.set_left_out(snapshot.left_out);
    cache.note_left_out_temporary(snapshot.left_out_temporary);
    settle(&mut cache);
    assert!(read_times(&mut cache, 1, 30).iter().all(|flip| !flip));

    // A left-out rule that is not temporary does not.
    let mut huge = rule("huge", "always", true);
    huge.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    assert!(!bounded_snapshot(vec![huge]).left_out_temporary);
    // A fresh snapshot forgets it.
    cache.replace_all(vec![rule("a", "always", true)]);
    cache.set_left_out(Default::default());
    settle(&mut cache);
    let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_RAISE));
    assert_eq!(flips.last(), Some(&true));
}

#[test]
fn a_reading_taken_while_settling_is_not_evidence() {
    let mut cache = synced(2);
    settle(&mut cache);
    for _ in 0..20 {
        let flip = cache.observe_rule_count(Reading {
            reported: 3,
            uptime: 100,
            settling: true,
            now: Instant::now(),
        });
        assert!(!flip);
    }
    // Readings taken while settling leave no run behind.
    assert_eq!(read_times(&mut cache, 3, 2), vec![false, false]);
}

/// A disagreement has to hold with nothing in between: a pause is a gap.
#[test]
fn a_pause_interrupts_a_run() {
    let mut cache = synced(2);
    settle(&mut cache);
    assert_eq!(read_times(&mut cache, 3, 2), vec![false, false]);
    let paused = cache.observe_rule_count(Reading {
        reported: 3,
        uptime: 100,
        settling: true,
        now: Instant::now(),
    });
    assert!(!paused);
    assert_eq!(read_times(&mut cache, 3, 2), vec![false, false]);
    assert!(read(&mut cache, 3));
}

#[test]
fn a_hint_already_shown_stays_while_the_check_is_paused_and_the_disagreement_holds() {
    let mut cache = synced(2);
    raised(&mut cache, 3);
    // A timed rule is made: the list changes (quiet), then the check pauses.
    cache.upsert(rule("timed", "5m", true));
    settle(&mut cache);
    // Still more than the list: nothing changes, and nothing new is raised.
    assert!(read_times(&mut cache, 4, 30).iter().all(|flip| !flip));
    assert!(cache.count_mismatch());
    // It ends: three agreeing readings clear it.
    cache.remove("timed");
    settle(&mut cache);
    let flips = read_times(&mut cache, 2, usize::from(PINGS_TO_CLEAR));
    assert_eq!(flips.last(), Some(&true));
    assert!(!cache.count_mismatch());
}

#[test]
fn a_staged_snapshot_nobody_has_adopted_is_pending_until_it_is_adopted_or_stale() {
    let mut pending = PendingSnapshots::default();
    let now = Instant::now();
    assert!(!pending.awaiting_adoption(now));
    pending.stage(None, vec![rule("a", "always", true)], now);
    assert!(pending.awaiting_adoption(now));
    assert!(pending.awaiting_adoption(now + Duration::from_secs(29)));
    assert!(!pending.awaiting_adoption(now + Duration::from_secs(31)));
    pending.adopt_fresh(&None, 1, now);
    assert!(!pending.awaiting_adoption(now), "adopted: nothing waits");
}

fn sync_with(rules: usize) -> (RulesSync, broadcast::Receiver<ServerMessage>) {
    let (tx, rx) = broadcast::channel(64);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).replace_all(
        (0..rules)
            .map(|i| rule(&format!("r{i}"), "always", true))
            .collect(),
    );
    (sync, rx)
}

fn sent(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn ping(sync: &RulesSync, reported: u64) {
    sync.observe_daemon_rules(reported, 100, 0);
}

/// Never per ping: one message when the hint comes, one when it goes, and
/// neither is a list.
#[test]
fn the_hint_is_broadcast_once_per_flip_and_never_with_a_list() {
    let (sync, mut rx) = sync_with(2);
    for _ in 0..usize::from(QUIET_PINGS) {
        ping(&sync, 2);
    }
    assert!(sent(&mut rx).is_empty());
    for _ in 0..usize::from(PINGS_TO_RAISE) - 1 {
        ping(&sync, 3);
    }
    assert!(sent(&mut rx).is_empty(), "not before the last reading");
    ping(&sync, 3);
    let on = sent(&mut rx);
    assert!(
        matches!(
            on.as_slice(),
            [ServerMessage::RulesNotShown {
                count_mismatch: true,
                listed: true,
                ..
            }]
        ),
        "{on:?}"
    );
    for _ in 0..30 {
        ping(&sync, 3);
    }
    assert!(sent(&mut rx).is_empty(), "it holds without a word");
    for _ in 0..usize::from(PINGS_TO_CLEAR) {
        ping(&sync, 2);
    }
    let off = sent(&mut rx);
    assert!(
        matches!(
            off.as_slice(),
            [ServerMessage::RulesNotShown {
                count_mismatch: false,
                ..
            }]
        ),
        "{off:?}"
    );
    for _ in 0..30 {
        ping(&sync, 2);
    }
    assert!(sent(&mut rx).is_empty());
}

#[test]
fn a_command_still_waiting_for_its_reply_pauses_the_check() {
    let (sync, mut rx) = sync_with(2);
    for _ in 0..usize::from(QUIET_PINGS) {
        ping(&sync, 2);
    }
    for _ in 0..30 {
        sync.observe_daemon_rules(3, 100, 1);
    }
    assert!(sent(&mut rx).is_empty());
    assert!(!lock(&sync.cache).count_mismatch());
    // Replied: the daemon's 3 now stands against a list of 2.
    for _ in 0..usize::from(PINGS_TO_RAISE) {
        ping(&sync, 3);
    }
    assert!(lock(&sync.cache).count_mismatch());
}

#[test]
fn a_snapshot_waiting_for_its_hello_pauses_the_check() {
    let (sync, mut rx) = sync_with(2);
    for _ in 0..usize::from(QUIET_PINGS) {
        ping(&sync, 2);
    }
    sync.stage(None, vec![rule("a", "always", true); 3]);
    for _ in 0..30 {
        ping(&sync, 3);
    }
    assert!(sent(&mut rx).is_empty());
    assert!(!lock(&sync.cache).count_mismatch());
}

/// A GUI that connects later, or asks for a snapshot, learns the state.
#[test]
fn every_published_list_carries_the_hint_and_a_new_snapshot_drops_it() {
    let (sync, mut rx) = sync_with(2);
    for _ in 0..usize::from(QUIET_PINGS) {
        ping(&sync, 2);
    }
    for _ in 0..usize::from(PINGS_TO_RAISE) {
        ping(&sync, 3);
    }
    sent(&mut rx);
    sync.publish();
    let published = sent(&mut rx);
    assert!(
        matches!(
            published.as_slice(),
            [
                ServerMessage::SetRules { .. },
                ServerMessage::RulesNotShown {
                    count_mismatch: true,
                    ..
                }
            ]
        ),
        "{published:?}"
    );
    lock(&sync.cache).replace_all(vec![rule("a", "always", true)]);
    sync.publish();
    assert!(
        matches!(
            sent(&mut rx).as_slice(),
            [
                ServerMessage::SetRules { .. },
                ServerMessage::RulesNotShown {
                    count_mismatch: false,
                    ..
                }
            ]
        ),
        "an adopted list starts over"
    );
}

fn not_shown(count_mismatch: bool) -> ServerMessage {
    ServerMessage::RulesNotShown {
        too_large: 0,
        over_limit_total: None,
        listed: true,
        left_on_disk: 0,
        count_mismatch,
    }
}

/// Additive and optional: absent unless true, so a bridge with nothing to
/// say sends exactly what it always did.
#[test]
fn the_wire_field_is_omitted_when_false_and_present_when_true() {
    let off = serde_json::to_value(not_shown(false)).unwrap();
    assert!(off.get("countMismatch").is_none(), "{off}");
    let on = serde_json::to_value(not_shown(true)).unwrap();
    assert_eq!(on["countMismatch"], true, "{on}");
    assert_eq!(on["action"], "rulesNotShown");
}

#[test]
fn an_older_bridges_frame_reads_as_no_mismatch_and_a_newer_one_is_read() {
    let old: ServerMessage =
        serde_json::from_str(r#"{"action":"rulesNotShown","tooLarge":0,"listed":true}"#).unwrap();
    assert_eq!(old, not_shown(false));
    let new: ServerMessage = serde_json::from_str(
        r#"{"action":"rulesNotShown","tooLarge":0,"listed":true,"countMismatch":true}"#,
    )
    .unwrap();
    assert_eq!(new, not_shown(true));
}

/// What an older GUI does with this bridge's frame: fields it doesn't know
/// are ignored, not an error (a `deny_unknown_fields` would break it).
#[test]
fn a_reader_ignores_fields_it_does_not_know() {
    let frame = r#"{"action":"rulesNotShown","tooLarge":0,"listed":true,"someFutureField":1}"#;
    assert_eq!(
        serde_json::from_str::<ServerMessage>(frame).unwrap(),
        not_shown(false)
    );
}
