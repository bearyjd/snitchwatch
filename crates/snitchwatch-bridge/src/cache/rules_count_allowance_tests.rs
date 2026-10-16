//! Tests for the rules the daemon may hold that the list does not show
//! (`MayHold`, review of PR #124): a prompt answer stored as `<name>-N`, a
//! refused add the daemon applied anyway, and a temporary rule the bridge
//! pruned while the daemon's monotonic timer was still running. In each, a
//! count above the list's, up to the allowance, is agreement.

use super::*;
use crate::cache::rules::{lock, now_secs};
use crate::daemon_commands::{DaemonCommands, DaemonTransport};
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Operator, Rule,
};
use std::time::Duration;
use tokio::sync::broadcast;

const T: i64 = 1_800_000_000;

fn rule(name: &str, duration: &str, enabled: bool) -> Rule {
    Rule {
        created: T,
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

fn synced(rules: Vec<Rule>) -> RulesCache {
    let mut cache = RulesCache::default();
    cache.replace_all(rules);
    cache
}

fn read_at(cache: &mut RulesCache, reported: u64, now: Instant) -> bool {
    cache.observe_rule_count(Reading {
        reported,
        uptime: 100,
        settling: false,
        now,
    })
}

fn read(cache: &mut RulesCache, reported: u64) -> bool {
    read_at(cache, reported, Instant::now())
}

fn many(cache: &mut RulesCache, reported: u64, times: usize) -> Vec<bool> {
    (0..times).map(|_| read(cache, reported)).collect()
}

/// The quiet readings after a change, spent on counts that agree.
fn settle(cache: &mut RulesCache, reported: u64) {
    for _ in 0..QUIET_PINGS {
        assert!(!read(cache, reported));
    }
}

fn none_flipped(flips: &[bool]) -> bool {
    flips.iter().all(|flip| !flip)
}

fn change(rule: Rule) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule],
        ..Default::default()
    }
}

// --- H1: a prompt answer under a name the daemon already holds ----------

/// The daemon stores a prompt answer through `addUserRule` →
/// `setUniqueName`: under `<name>-2` when `<name>` is in memory, a disabled
/// remembered rule included. The bridge replaces `<name>`, so the daemon
/// holds one more rule than the list, for good.
#[test]
fn a_prompt_answer_under_a_listed_name_lets_the_daemon_hold_one_more() {
    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 3);
    assert!(
        none_flipped(&many(&mut cache, 3, 40)),
        "the daemon has seen-2"
    );
    assert!(!cache.count_mismatch());
    // The daemon never stored it: that agrees too.
    assert!(none_flipped(&many(&mut cache, 2, 40)));
    // One more than that is not explained.
    let flips = many(&mut cache, 4, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// The daemon holds the left-out rule (a name the list doesn't show) or the
/// refused add it stored anyway, and the answer is saved as `<name>-2`:
/// the list then shows `<name>` once, and the daemon has two.
#[test]
fn a_prompt_answer_under_a_left_out_name_allows_one_more() {
    let mut cache = synced(vec![rule("other", "always", true)]);
    cache.set_left_out([("big".to_string(), 20_000)].into());
    cache.note_prompt_answer("big");
    cache.upsert(rule("big", "always", true));
    settle(&mut cache, 3);
    assert!(none_flipped(&many(&mut cache, 3, 40)), "other, big, big-2");
    let flips = many(&mut cache, 4, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn a_prompt_answer_under_the_name_of_a_refused_add_allows_one_more() {
    let mut cache = synced(vec![rule("other", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    cache.note_prompt_answer("c");
    cache.upsert(rule("c", "always", true));
    settle(&mut cache, 3);
    assert!(none_flipped(&many(&mut cache, 3, 40)), "other, c, c-2");
    let flips = many(&mut cache, 4, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn a_prompt_answer_under_a_new_name_allows_nothing() {
    let mut cache = synced(vec![rule("other", "always", true)]);
    cache.note_prompt_answer("fresh");
    cache.upsert(rule("fresh", "always", true));
    settle(&mut cache, 2);
    let flips = many(&mut cache, 3, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn every_answer_that_meets_a_listed_name_allows_one_more() {
    let mut cache = synced(vec![rule("seen", "always", false)]);
    for _ in 0..3 {
        cache.note_prompt_answer("seen");
        cache.upsert(rule("seen", "always", true));
    }
    settle(&mut cache, 4);
    assert!(none_flipped(&many(&mut cache, 4, 40)), "seen-2, -3, -4");
    let flips = many(&mut cache, 5, 3);
    assert_eq!(flips.last(), Some(&true));
}

/// Through the path `ask_rule` takes.
#[test]
fn the_ask_rule_path_notes_the_collision() {
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).replace_all(vec![rule("seen", "always", false)]);
    sync.upsert(rule("seen", "always", true));
    for _ in 0..QUIET_PINGS {
        sync.observe_daemon_rules(2, 100, 0);
    }
    for _ in 0..40 {
        sync.observe_daemon_rules(2, 100, 0);
    }
    assert!(!lock(&sync.cache).count_mismatch());
}

/// "For 5 minutes" over a disabled remembered rule: the bridge's rule
/// replaces `<name>` and is pruned at its expiry; the daemon keeps the
/// disabled original, so it holds one more, the other way round.
#[test]
fn a_timed_answer_over_a_disabled_rule_leaves_the_daemon_one_more_after_expiry() {
    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert_at(rule("seen", "5m", true), T);
    // While it is listed the check is paused.
    settle(&mut cache, 3);
    assert!(none_flipped(&many(&mut cache, 3, 20)));
    // Expired: the daemon's timer and the bridge's agree (no suspend).
    let ends = Instant::now() + Duration::from_secs(300);
    assert_eq!(
        cache.prune_expired_at(T + 300, ends + Duration::from_secs(1)),
        vec!["seen"]
    );
    settle(&mut cache, 2);
    // The daemon: other, and the disabled original.
    assert!(none_flipped(&many(&mut cache, 2, 40)));
    assert!(!cache.count_mismatch());
}

/// #120 item 29: the daemon's count settling below the room a prompt answer
/// made means the extra rule went (a `<name>-2` removed outside the bridge),
/// so the room goes too, and an outside add after it is said.
#[test]
fn the_room_a_prompt_answer_made_shrinks_when_the_daemon_holds_fewer() {
    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 3);
    assert!(
        none_flipped(&many(&mut cache, 3, 10)),
        "the daemon has seen-2"
    );
    // seen-2.json removed by hand: the daemon holds what the list shows.
    assert!(none_flipped(&many(&mut cache, 2, PINGS_TO_CLEAR.into())));
    // A rule added outside the bridge is no longer hidden by that room.
    let flips = many(&mut cache, 3, 6);
    assert!(flips.contains(&true), "{flips:?}");
    assert!(cache.count_mismatch());
}

/// A listed temporary rule's timer may drop it unseen, so a lower count may
/// hide `seen-2` behind it: the room is kept, and the count is no evidence.
/// And one lower reading is not a run.
#[test]
fn the_room_a_prompt_answer_made_is_kept_for_a_timer_or_a_single_reading() {
    let mut cache = synced(vec![
        rule("other", "5m", false),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 2);
    assert!(none_flipped(&many(&mut cache, 2, 10)));
    assert!(
        none_flipped(&many(&mut cache, 3, 10)),
        "room for seen-2 is kept"
    );

    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 3);
    assert!(none_flipped(&many(&mut cache, 3, 5)));
    assert!(!read(&mut cache, 2));
    assert!(
        none_flipped(&many(&mut cache, 3, 10)),
        "one low reading kept the room"
    );
    assert!(!cache.count_mismatch());
}

/// A raised hint still clears while a listed temporary rule's timer may
/// drop it unseen, but that agreement is no evidence the answers' room went:
/// the daemon may have dropped the temporary rule, not `seen-2` (PR #150
/// review M1).
#[test]
fn a_hint_clearing_while_a_timer_may_drop_rules_keeps_the_answers_room() {
    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 4);
    let flips = many(&mut cache, 4, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
    // A temporary rule; the daemon's timer drops it before the bridge's.
    cache.upsert(rule("t", "5m", true));
    settle(&mut cache, 3);
    let flips = many(&mut cache, 3, 6);
    assert!(flips.contains(&true), "the hint clears: {flips:?}");
    assert!(!cache.count_mismatch());
    // The bridge prunes it: the daemon still holds seen-2.
    cache.remove("t");
    settle(&mut cache, 3);
    assert!(
        none_flipped(&many(&mut cache, 3, 10)),
        "seen-2 is still room"
    );
    assert!(!cache.count_mismatch());
}

/// Room for a refused add is room for more, never fewer: with it, the
/// prompt answers' room shrinks to what the daemon holds over the list,
/// and the refused add keeps its own.
#[test]
fn a_refused_add_keeps_its_own_room_when_the_answers_room_shrinks() {
    let mut cache = synced(vec![
        rule("other", "always", true),
        rule("seen", "always", false),
    ]);
    cache.apply_refused(&change(rule("refused", "always", true)));
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 2);
    assert!(none_flipped(&many(&mut cache, 2, 10)));
    assert!(none_flipped(&many(&mut cache, 3, 10)), "the refused add");
    let flips = many(&mut cache, 4, 6);
    assert!(flips.contains(&true), "{flips:?}");
}

// --- M1: a refused add the daemon applied anyway -------------------------

/// Stock `Replace` stores a rule in memory before `Save` can fail, and
/// `scheduleTemporaryRule` fails on a bad duration after storing it: an
/// `ERROR` for an add may leave the rule in the daemon.
#[test]
fn a_refused_add_may_have_been_applied() {
    let mut cache = synced(vec![rule("a", "always", true), rule("b", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    settle(&mut cache, 3);
    assert!(none_flipped(&many(&mut cache, 3, 40)));
    assert!(none_flipped(&many(&mut cache, 2, 40)), "or it was not");
    let flips = many(&mut cache, 4, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// The allowance is room for rules the list doesn't show, never for fewer
/// than it shows: a daemon that holds less is still said.
#[test]
fn the_allowance_only_goes_upward() {
    let mut cache = synced(vec![rule("a", "always", true), rule("b", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    settle(&mut cache, 2);
    let flips = many(&mut cache, 1, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");

    let mut cache = synced(vec![
        rule("seen", "always", false),
        rule("b", "always", true),
    ]);
    cache.note_prompt_answer("seen");
    cache.upsert(rule("seen", "always", true));
    settle(&mut cache, 2);
    let flips = many(&mut cache, 1, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn a_refused_temporary_add_with_a_duration_go_cannot_parse_may_have_been_applied() {
    let mut cache = synced(vec![rule("a", "always", true)]);
    cache.apply_refused(&change(rule("c", "5 minutes", true)));
    settle(&mut cache, 2);
    assert!(none_flipped(&many(&mut cache, 2, 40)));
}

#[test]
fn a_refused_change_of_a_listed_rule_allows_nothing() {
    let mut cache = synced(vec![rule("a", "always", true)]);
    cache.apply_refused(&change(rule("a", "always", true)));
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn the_same_refused_add_twice_allows_one_and_a_later_ok_allows_none() {
    let mut cache = synced(vec![rule("a", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    cache.apply_refused(&change(rule("c", "always", true)));
    settle(&mut cache, 2);
    assert!(none_flipped(&many(&mut cache, 2, 20)));
    let flips = many(&mut cache, 3, 3);
    assert_eq!(flips.last(), Some(&true), "one name is one rule: {flips:?}");

    // The retry works: the list has it, and nothing more is allowed.
    let mut cache = synced(vec![rule("a", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    cache.apply_confirmed(&change(rule("c", "always", true)));
    settle(&mut cache, 2);
    let flips = many(&mut cache, 3, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// The daemon deleted what a refused add had put in (the curated defaults
/// do this when switched off): its copy is gone, and so is the allowance.
#[test]
fn a_confirmed_delete_of_a_refused_add_forgets_it() {
    let mut cache = synced(vec![rule("a", "always", true)]);
    cache.apply_refused(&change(rule("c", "always", true)));
    cache.apply_confirmed(&Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![rule("c", "always", true)],
        ..Default::default()
    });
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn only_so_many_refused_names_are_remembered() {
    let mut cache = synced(Vec::new());
    for i in 0..(MAX_MAYBE_APPLIED + 10) {
        cache.apply_refused(&change(rule(&format!("c{i}"), "always", true)));
    }
    assert_eq!(cache.may_hold.refused.len(), MAX_MAYBE_APPLIED);
}

// --- M2: a pruned temporary rule the daemon's monotonic timer still holds -

/// The wall clock ran on while the host slept, the daemon's monotonic timer
/// did not: the bridge pruned a rule the daemon still holds, until its timer
/// fires.
#[test]
fn a_rule_pruned_before_the_daemons_timer_fires_may_still_be_in_the_daemon() {
    let mut cache = synced(vec![rule("other", "always", true)]);
    cache.upsert_at(rule("timed", "5m", true), T);
    let started = Instant::now();
    // Resumed after a long sleep: the wall clock says it is over, the
    // monotonic clock has run 100 s of the 300.
    assert_eq!(
        cache.prune_expired_at(T + 3_000, started + Duration::from_secs(100)),
        vec!["timed"]
    );
    let during = started + Duration::from_secs(150);
    settle(&mut cache, 1);
    for _ in 0..40 {
        assert!(!read_at(&mut cache, 2, during), "the daemon still has it");
    }
    assert!(!cache.count_mismatch());
    // Once its timer has fired (and a little more), the extra rule is not
    // explained.
    let later = started + Duration::from_secs(300 + 60);
    let flips: Vec<bool> = (0..3).map(|_| read_at(&mut cache, 2, later)).collect();
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

#[test]
fn a_rule_pruned_when_its_timer_has_fired_allows_nothing() {
    let mut cache = synced(vec![rule("other", "always", true)]);
    cache.upsert_at(rule("timed", "5m", true), T);
    let started = Instant::now();
    cache.prune_expired_at(T + 300, started + Duration::from_secs(301));
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// A snapshot's temporary rule has the same two clocks.
#[test]
fn a_snapshots_temporary_rule_pruned_early_may_still_be_in_the_daemon() {
    let created = now_secs();
    let mut cache = synced(vec![
        rule("other", "always", true),
        Rule {
            created,
            ..rule("timed", "1h", true)
        },
    ]);
    let started = Instant::now();
    cache.prune_expired_at(created + 3_600, started + Duration::from_secs(60));
    settle(&mut cache, 1);
    let at = started + Duration::from_secs(120);
    for _ in 0..40 {
        assert!(!read_at(&mut cache, 2, at));
    }
}

/// Through the path a ping takes.
#[test]
fn the_ping_path_tolerates_a_rule_pruned_before_the_daemons_timer_fires() {
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let sync = RulesSync::new(tx);
    {
        let mut cache = lock(&sync.cache);
        cache.replace_all(vec![rule("other", "always", true)]);
        cache.upsert(rule("timed", "5m", true));
        // The host slept: the wall clock is far ahead, the monotonic one not.
        cache.prune_expired_at(now_secs() + 3_000, Instant::now());
        assert!(!cache.contains("timed"));
    }
    for _ in 0..usize::from(QUIET_PINGS) + 40 {
        sync.observe_daemon_rules(2, 100, 0);
    }
    assert!(!lock(&sync.cache).count_mismatch());
    assert!(hint_messages(&mut rx).iter().all(|shown| !shown));
}

/// A snapshot's temporary rule has a stamp from before the snapshot: a file
/// loaded at daemon start (the daemon's timer starts at load, `loadRule`) or
/// a list adopted after the host slept. Its wall-clock expiry may be long
/// past while the daemon's timer has its whole duration to run.
#[test]
fn a_snapshots_temporary_rule_with_an_old_stamp_may_still_have_its_whole_duration_in_the_daemon() {
    let created = now_secs() - 3_000;
    let mut cache = synced(vec![
        rule("other", "always", true),
        Rule {
            created,
            ..rule("timed", "5m", true)
        },
    ]);
    let started = Instant::now();
    assert_eq!(cache.prune_expired_at(now_secs(), started), vec!["timed"]);
    settle(&mut cache, 1);
    let during = started + Duration::from_secs(120);
    for _ in 0..40 {
        assert!(
            !read_at(&mut cache, 2, during),
            "the daemon's timer still runs"
        );
    }
    assert!(!cache.count_mismatch());
    // Past its whole duration (and a little more) the extra rule isn't explained.
    let later = started + Duration::from_secs(300 + 60);
    let flips: Vec<bool> = (0..3).map(|_| read_at(&mut cache, 2, later)).collect();
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

/// The end of a timer near the limit of the clock must not panic inside the
/// cache lock.
#[test]
fn a_pruned_rule_ending_at_the_edge_of_the_clock_does_not_panic() {
    let now = Instant::now();
    let (mut lo, mut hi) = (0_u64, u64::MAX / 2);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if now.checked_add(Duration::from_secs(mid)).is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let far = now + Duration::from_secs(lo);
    assert!(
        far.checked_add(Duration::from_secs(5)).is_none(),
        "at the edge"
    );
    let mut hold = MayHold::default();
    hold.note_pruned(far, now);
    assert_eq!(hold.allowance(now), 1);
}

// --- the allowance ends with the list ------------------------------------

#[test]
fn a_new_snapshot_and_a_withdrawal_forget_the_allowance() {
    let mut cache = synced(vec![rule("seen", "always", false)]);
    cache.note_prompt_answer("seen");
    cache.apply_refused(&change(rule("c", "always", true)));
    cache.replace_all(vec![rule("seen", "always", false)]);
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");

    let mut cache = synced(vec![rule("seen", "always", false)]);
    cache.note_prompt_answer("seen");
    cache.set_unknown();
    assert_eq!(
        cache.may_hold.allowance(Instant::now()),
        0,
        "withdrawn at once"
    );
    cache.replace_all(vec![rule("seen", "always", false)]);
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true), "{flips:?}");
}

// --- L: a hint already showing can clear while the check is paused -------

/// A temporary rule that never gets an expiry (a duration the bridge cannot
/// parse) pauses the check for good. It must not keep a hint up.
#[test]
fn a_hint_can_clear_while_a_temporary_rule_pauses_the_check_but_not_be_raised() {
    let mut cache = synced(vec![rule("a", "always", true)]);
    settle(&mut cache, 1);
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips.last(), Some(&true));
    // A rule with a duration only the daemon can read joins the list.
    cache.upsert(rule("typed", "1.5h", true));
    settle(&mut cache, 2);
    assert!(
        none_flipped(&many(&mut cache, 3, 20)),
        "disagreement, paused"
    );
    assert!(cache.count_mismatch(), "and it stays while it holds");
    let flips = many(&mut cache, 2, 3);
    assert_eq!(flips, vec![false, false, true], "agreement clears it");
    assert!(!cache.count_mismatch());
    // Not raised again while paused.
    assert!(none_flipped(&many(&mut cache, 5, 40)));
}

// --- L: adoption and re-adoption on the Unix socket ----------------------

fn hello() -> NotificationReply {
    NotificationReply {
        id: 0,
        code: NotificationReplyCode::Ok as i32,
        data: String::new(),
    }
}

fn ok(id: u64) -> NotificationReply {
    NotificationReply {
        id,
        code: NotificationReplyCode::Ok as i32,
        data: String::new(),
    }
}

fn hint_messages(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<bool> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|message| match message {
            ServerMessage::RulesNotShown { count_mismatch, .. } => Some(count_mismatch),
            _ => None,
        })
        .collect()
}

/// A redialled daemon's old and new streams share the socket's one key. A
/// second adoption replaces the list with the staged snapshot plus what was
/// confirmed in between (its journal, #115), so the list still matches the
/// daemon's count. The watch treats each adoption as a new baseline (flag
/// cleared, readings quiet), and with the list right never raises.
#[tokio::test]
async fn each_adoption_on_the_unix_socket_is_a_new_baseline_for_the_hint() {
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(64);
    let rules = RulesSync::new(tx);
    let commands = DaemonCommands::new(DaemonTransport::Unix, rules.clone());
    rules.stage(
        None,
        vec![rule("a", "always", true), rule("b", "always", true)],
    );
    let (first, _first_rx) = commands.open_stream(None);
    commands.on_reply(first.id(), &hello());
    let ping = |reported: u64| rules.observe_daemon_rules(reported, 100, 0);
    for _ in 0..QUIET_PINGS {
        ping(2);
    }
    hint_messages(&mut rx);

    // A command confirmed on the first stream: the daemon has c, so has the list.
    let pending = commands.send(change(rule("c", "always", true))).unwrap();
    commands.on_reply(first.id(), &ok(pending.id()));
    for _ in 0..usize::from(QUIET_PINGS) + 20 {
        ping(3);
    }
    assert!(hint_messages(&mut rx).iter().all(|shown| !shown));

    // The new stream's HELLO adopts the staged snapshot again, which lacks
    // c, and replays c from its journal.
    let (second, _second_rx) = commands.open_stream(None);
    commands.on_reply(second.id(), &hello());
    assert!(!rules.cache().lock().unwrap().count_mismatch());
    assert_eq!(
        rules.cache().lock().unwrap().rules().unwrap().len(),
        3,
        "c is kept"
    );
    for _ in 0..usize::from(QUIET_PINGS) + usize::from(PINGS_TO_RAISE) {
        ping(3);
    }
    assert!(
        hint_messages(&mut rx).iter().all(|shown| !shown),
        "the list matches the daemon"
    );

    // The second stream closes: the first, current again, holds the same
    // snapshot, and that is another new baseline.
    drop(second);
    assert!(!rules.cache().lock().unwrap().count_mismatch());
    for _ in 0..QUIET_PINGS {
        ping(3);
    }
    assert!(hint_messages(&mut rx).iter().all(|shown| !shown));
}

/// PR #166 L3: a journal over its cap isn't replayed, so a second HELLO
/// adopts the bare snapshot. The daemon still holds what was confirmed in
/// between, the list doesn't, and after the new baseline the hint says so.
#[tokio::test]
async fn an_overflowed_journal_adopts_the_bare_snapshot_and_the_hint_raises() {
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(1024);
    let rules = RulesSync::new(tx);
    let commands = DaemonCommands::new(DaemonTransport::Unix, rules.clone());
    rules.stage(None, vec![rule("a", "always", true)]);
    let (first, _first_rx) = commands.open_stream(None);
    commands.on_reply(first.id(), &hello());
    let changes = crate::cache::rules::MAX_JOURNAL + 1;
    // Confirmed on the first stream (as `on_reply` applies an `OK`).
    for i in 0..changes {
        rules.apply_confirmed(&change(rule(&format!("r{i}"), "always", true)));
    }
    let held = u64::try_from(1 + changes).unwrap();

    let (second, _second_rx) = commands.open_stream(None);
    commands.on_reply(second.id(), &hello());
    assert_eq!(rules.cache().lock().unwrap().rules().unwrap().len(), 1);
    hint_messages(&mut rx);
    for _ in 0..usize::from(QUIET_PINGS) + usize::from(PINGS_TO_RAISE) {
        rules.observe_daemon_rules(held, 100, 0);
    }
    assert_eq!(hint_messages(&mut rx), vec![true], "the list lost them");
    drop(second);
    drop(first);
}

/// PR #166 I3: a remembered prompt answer is replayed on re-adoption with
/// its rename allowance: the daemon stored it as `<name>-2`, and the list
/// shows the answer under `<name>`.
#[tokio::test]
async fn a_replayed_prompt_answer_keeps_its_rename_allowance() {
    let (tx, _rx) = broadcast::channel::<ServerMessage>(64);
    let rules = RulesSync::new(tx);
    let commands = DaemonCommands::new(DaemonTransport::Unix, rules.clone());
    rules.stage(
        None,
        vec![rule("other", "always", true), rule("seen", "always", false)],
    );
    let (first, _first_rx) = commands.open_stream(None);
    commands.on_reply(first.id(), &hello());
    rules.upsert(rule("seen", "always", true));
    assert_eq!(rules.cache().lock().unwrap().may_hold.renamed, 1);

    let (second, _second_rx) = commands.open_stream(None);
    commands.on_reply(second.id(), &hello());
    let cache = rules.cache();
    let cache = cache.lock().unwrap();
    assert!(
        cache.rules().unwrap()["seen"].enabled,
        "the answer is listed"
    );
    assert_eq!(cache.may_hold.renamed, 1, "counted once, as it was");
    drop(cache);
    drop(second);
    drop(first);
}
