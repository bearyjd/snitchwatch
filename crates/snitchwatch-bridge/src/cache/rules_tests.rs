//! Tests for [`super`]: the cache, staged snapshots and their limits.

use super::*;
use crate::rule_wire::{rule_from_wire, rule_to_wire};
use snitchwatch_proto::protocol::Operator;

const T: i64 = 1_800_000_000;

fn rule(name: &str, duration: &str, created: i64) -> Rule {
    Rule {
        created,
        name: name.to_string(),
        enabled: true,
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

fn names(cache: &RulesCache) -> Vec<String> {
    cache
        .snapshot_wire()
        .expect("synced")
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect()
}

fn get<'a>(cache: &'a RulesCache, name: &str) -> &'a Rule {
    &cache.rules().expect("cache is Unknown")[name]
}

#[test]
fn unknown_yields_no_snapshot_and_synced_empty_yields_an_empty_one() {
    assert_eq!(RulesCache::default().snapshot_wire(), None);
    assert_eq!(synced(Vec::new()).snapshot_wire(), Some(Vec::new()));
}

#[test]
fn replace_all_snapshot_is_name_sorted() {
    let cache = synced(vec![
        rule("b-rule", "always", 0),
        rule("000-first", "always", 0),
        rule("a-rule", "always", 0),
    ]);
    assert_eq!(names(&cache), vec!["000-first", "a-rule", "b-rule"]);
}

#[test]
fn upsert_and_remove_do_nothing_while_unknown() {
    let mut cache = RulesCache::default();
    cache.upsert(rule("a", "always", 0));
    cache.remove("a");
    assert!(cache.is_unknown());
}

/// A committed snapshot is the daemon's word on every rule's age, however old:
/// only a confirmed change (`upsert_at`) restamps.
#[test]
fn a_committed_snapshot_keeps_the_daemons_created() {
    let cache = synced(vec![
        rule("a", "always", T - 90 * 86_400),
        rule("b", "until restart", T - 5),
    ]);
    assert_eq!(get(&cache, "a").created, T - 90 * 86_400);
    assert_eq!(get(&cache, "b").created, T - 5);
}

/// `created` is the daemon's stamp, which it makes anew on every change
/// (`rule.Create` from each `CHANGE_RULE`): a change without one is stamped
/// with its time, and a stamp a rule brings is kept (PR #106 review M2).
#[test]
fn a_change_is_stamped_with_its_time_as_the_daemon_stamps_it() {
    let mut cache = synced(vec![rule("a", "5m", T - 100), rule("p", "always", 1)]);
    cache.upsert_at(rule("a", "5m", 0), T);
    assert_eq!(get(&cache, "a").created, T);
    cache.upsert_at(rule("p", "always", 0), T + 1);
    assert_eq!(get(&cache, "p").created, T + 1);
    cache.upsert_at(rule("a", "5m", T + 10), T + 20);
    assert_eq!(get(&cache, "a").created, T + 10);
    cache.upsert_at(rule("new", "always", 0), T);
    assert_eq!(names(&cache), vec!["a", "new", "p"]);
    cache.remove("a");
    assert_eq!(names(&cache), vec!["new", "p"]);
}

/// The daemon rebuilds a rule from every `CHANGE_RULE` (`rule.Create` stamps
/// `Created` with the time of the change), so a permanent rule the GUI edits
/// or re-enables is new as far as its age goes. Keeping the old `created` made
/// a 40-day-old rule just edited read "unused for 14 days" at once (the
/// Rules page's badge needs the age).
#[test]
fn a_permanent_rule_the_gui_changes_is_stamped_now_as_the_daemon_stamps_it() {
    for duration in ["always", "until restart"] {
        let mut cache = synced(vec![rule("a", duration, T - 40 * 86_400)]);
        cache.upsert_at(rule("a", duration, 0), T);
        assert_eq!(get(&cache, "a").created, T, "{duration}");
    }
}

#[test]
fn a_permanent_rule_new_to_the_cache_without_a_stamp_is_stamped_now_too() {
    let mut cache = synced(Vec::new());
    cache.upsert_at(rule("fresh", "always", 0), T);
    assert_eq!(get(&cache, "fresh").created, T);
}

#[test]
fn a_stamp_the_rule_already_has_is_kept() {
    let mut cache = synced(vec![rule("a", "always", T - 100)]);
    cache.upsert_at(rule("a", "always", T - 5), T);
    assert_eq!(get(&cache, "a").created, T - 5);
}

/// #101 and PR #106 review M2 together: every confirmed change restamps
/// `created`, permanent or timed, as the daemon does; a timed rule's clock is
/// apart from it, and one of the same duration keeps running.
#[test]
fn a_confirmed_change_restamps_every_rule_but_a_timed_ones_clock_keeps_running() {
    let mut cache = synced(vec![
        rule("perm", "always", T - 40 * 86_400),
        rule("timed", "5m", T - 100),
    ]);
    let change = |name: &str, duration: &str| Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule(name, duration, 0)],
        ..Default::default()
    };
    cache.apply_confirmed_at(&change("perm", "always"), T);
    cache.apply_confirmed_at(&change("timed", "5m"), T);
    assert_eq!(get(&cache, "perm").created, T);
    assert_eq!(get(&cache, "timed").created, T);
    assert_eq!(cache.expiry_of("perm"), None);
    assert_eq!(cache.expiry_of("timed"), Some(T + 200));
}

/// A changed duration doesn't keep the old clock (`scheduleTemporaryRule`
/// ignores a timer whose duration no longer matches): an enabled rule's
/// starts at the change, which is also its new `created`.
#[test]
fn a_changed_duration_does_not_keep_the_old_clock() {
    let mut cache = synced(vec![rule("a", "5m", T)]);
    cache.upsert_at(rule("a", "1h", 0), T + 60);
    assert_eq!(get(&cache, "a").created, T + 60);
    assert_eq!(cache.expiry_of("a"), Some(T + 60 + 3600));
    assert!(
        cache.prune_expired(T + 301).is_empty(),
        "not on the old clock"
    );
}

#[test]
fn a_five_minute_rule_is_pruned_after_five_minutes() {
    let mut cache = synced(vec![rule("a", "5m", T - 301), rule("b", "5m", T - 299)]);
    assert_eq!(cache.prune_expired(T), vec!["a"], "the names it removed");
    assert_eq!(names(&cache), vec!["b"]);
    assert!(cache.prune_expired(T).is_empty(), "nothing left to prune");
}

#[test]
fn permanent_unparseable_and_undated_rules_never_expire() {
    let mut cache = synced(vec![
        rule("always", "always", 1),
        rule("restart", "until restart", 1),
        rule("once", "once", 1),
        rule("fractional", "1.5h", 1),
        rule("millis", "5ms", 1),
        rule("bare", "90", 1),
        rule("undated", "5m", 0),
    ]);
    assert!(cache.prune_expired(T).is_empty());
    assert_eq!(names(&cache).len(), 7);
}

#[test]
fn durations_parse_as_digit_unit_sequences() {
    assert_eq!(parse_duration_secs("30s"), Some(30));
    assert_eq!(parse_duration_secs("5m"), Some(300));
    assert_eq!(parse_duration_secs("1h30m"), Some(5400));
    for bad in ["", "m", "5", "5ms", "1.5h", "-5m", "5d", "5m "] {
        assert_eq!(parse_duration_secs(bad), None, "{bad:?}");
    }
}

/// The daemon's original timer deletes a toggled temporary rule on its
/// original schedule (`scheduleTemporaryRule`), so the cache must too,
/// although the toggle restamps `created`.
#[test]
fn a_toggled_temporary_rule_keeps_its_original_expiry() {
    let mut cache = synced(vec![rule("a", "5m", T)]);
    let mut toggled = rule_from_wire(&rule_to_wire(get(&cache, "a"))).unwrap();
    assert_eq!(toggled.created, 0, "rule_from_wire zeroes created");
    toggled.enabled = false;
    cache.upsert_at(toggled, T + 30);

    assert!(cache.prune_expired(T + 31).is_empty());
    assert!(!get(&cache, "a").enabled);
    assert_eq!(get(&cache, "a").created, T + 30);

    assert_eq!(cache.prune_expired(T + 301), vec!["a"]);
    assert_eq!(names(&cache), Vec::<String>::new());
}

#[test]
fn the_wire_round_trip_keeps_precedence_nolog_and_list_operators() {
    let mut original = rule("a", "always", T);
    original.precedence = true;
    original.nolog = true;
    original.operator = Some(Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: vec![
            Operator {
                r#type: "simple".into(),
                operand: "process.path".into(),
                data: "/usr/bin/curl".into(),
                sensitive: true,
                ..Default::default()
            },
            Operator {
                r#type: "regexp".into(),
                operand: "dest.host".into(),
                data: "^example\\.com$".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    });

    let back = rule_from_wire(&rule_to_wire(&original)).unwrap();

    assert!(back.precedence && back.nolog);
    let list = back.operator.unwrap().list;
    assert_eq!(list, original.operator.unwrap().list);
}

#[test]
fn apply_confirmed_upserts_changes_and_removes_deletes() {
    let mut cache = synced(vec![rule("a", "5m", T), rule("b", "always", T)]);
    let mut toggled = rule("a", "5m", 0);
    toggled.enabled = false;
    cache.apply_confirmed_at(
        &Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![toggled],
            ..Default::default()
        },
        T + 5,
    );
    assert!(!get(&cache, "a").enabled);
    assert_eq!(get(&cache, "a").created, T + 5);
    assert_eq!(cache.expiry_of("a"), Some(T + 300));

    cache.apply_confirmed(&Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: "b".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    assert_eq!(names(&cache), vec!["a"]);
}

fn key(port: u16) -> ConnKey {
    Some(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
}

/// A stream adopts its key's latest snapshot once; another stream of the
/// same key still can (PR #106 review OQ1).
#[test]
fn a_key_keeps_only_its_latest_snapshot_and_each_stream_adopts_it_once() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), vec![rule("old", "always", 0)], now);
    pending.stage(key(1), vec![rule("new", "always", 0)], now);

    let taken = pending.adopt_fresh(&key(1), 1, now).unwrap();
    assert_eq!(taken.rules[0].name, "new");
    assert_eq!(
        pending.adopt_fresh(&key(1), 1, now),
        None,
        "once per stream"
    );
    assert_eq!(
        pending.adopt_fresh(&key(1), 2, now).unwrap().rules[0].name,
        "new"
    );
}

#[test]
fn a_fifth_key_evicts_the_oldest() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    for port in 1..=5 {
        pending.stage(key(port), Vec::new(), now);
    }
    assert_eq!(pending.adopt_fresh(&key(1), 1, now), None);
    for port in 2..=5 {
        assert!(pending.adopt_fresh(&key(port), 1, now).is_some(), "{port}");
    }
}

#[test]
fn a_stale_snapshot_is_not_committed_and_is_removed() {
    let then = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), Vec::new(), then);

    assert_eq!(
        pending.adopt_fresh(&key(1), 1, then + Duration::from_secs(31)),
        None
    );
    assert!(pending.entries.is_empty());
}

#[test]
fn staging_evicts_stale_entries_of_other_keys() {
    let then = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), Vec::new(), then);
    pending.stage(key(2), Vec::new(), then + Duration::from_secs(31));
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(pending.entries[0].key, key(2));
}

fn nested(depth: usize) -> Operator {
    let leaf = rule("leaf", "always", 0).operator.unwrap();
    (1..depth).fold(leaf, |inner, _| Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: vec![inner],
        ..Default::default()
    })
}

#[test]
fn snapshots_and_rules_over_the_size_limits_are_not_staged() {
    let too_many = vec![rule("a", "always", 0); MAX_SNAPSHOT_RULES + 1];
    let counted = bounded_snapshot(too_many);
    assert_eq!(counted.over_limit, Some(MAX_SNAPSHOT_RULES + 1));
    assert!(counted.rules.is_empty() && counted.left_out.is_empty());

    let mut long = rule("long", "always", 0);
    long.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    let mut deep = rule("deep", "always", 0);
    deep.operator = Some(nested(MAX_OPERATOR_DEPTH + 1));
    let mut deepest_allowed = rule("deepest-allowed", "always", 0);
    deepest_allowed.operator = Some(nested(MAX_OPERATOR_DEPTH));
    let mut wide = rule("wide", "always", 0);
    wide.operator = Some(Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: vec![nested(1); MAX_OPERATOR_LIST_LEN + 1],
        ..Default::default()
    });

    let kept = bounded_snapshot(vec![
        rule("ok", "always", 0),
        long,
        deep,
        deepest_allowed,
        wide,
    ]);
    assert_eq!(kept.over_limit, None);
    let names: Vec<_> = kept.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["ok", "deepest-allowed"]);
}

/// An oversized snapshot replaces the connection's earlier one with its
/// count, and a list within the limits replaces the count (PR #106 L1).
#[test]
fn an_oversized_snapshot_and_a_list_replace_each_other_per_key() {
    let (tx, mut rx) = broadcast::channel(4);
    let sync = RulesSync::new(tx);
    sync.stage(key(1), vec![rule("a", "always", 0)]);
    sync.stage(key(1), vec![rule("a", "always", 0); MAX_SNAPSHOT_RULES + 1]);
    {
        let pending = lock(&sync.pending);
        assert_eq!(pending.entries.len(), 1);
        assert_eq!(
            pending.entries[0].snapshot.over_limit,
            Some(MAX_SNAPSHOT_RULES + 1)
        );
        assert!(pending.entries[0].snapshot.rules.is_empty());
    }
    assert!(
        published(&mut rx).is_empty(),
        "nothing shown before a HELLO"
    );
    sync.stage(key(1), vec![rule("a", "always", 0)]);
    let pending = lock(&sync.pending);
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(pending.entries[0].snapshot.over_limit, None);
}

#[test]
fn the_none_key_is_one_shared_key() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(None, vec![rule("a", "always", 0)], now);
    pending.stage(None, vec![rule("b", "always", 0)], now);
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(
        pending.adopt_fresh(&None, 1, now).unwrap().rules[0].name,
        "b"
    );
}

#[tokio::test(start_paused = true)]
async fn the_expiry_tick_prunes_and_publishes_then_ends_with_the_cache() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let cache: SharedRulesCache = Arc::new(StdMutex::new(synced(vec![
        rule("expired", "5m", now - 301),
        rule("kept", "always", 1),
    ])));
    let (tx, mut rx) = broadcast::channel(4);
    let tick = tokio::spawn(prune_expired_rules_every(
        Duration::from_secs(30),
        Arc::downgrade(&cache),
        tx.clone(),
        RuleHitsHandle::new(tx),
    ));

    let published = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("nothing was pruned");
    match published.unwrap() {
        ServerMessage::SetRules { rules } => {
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0]["name"], "kept");
        }
        other => panic!("expected SetRules, got {other:?}"),
    }

    drop(cache);
    tokio::time::timeout(Duration::from_secs(60), tick)
        .await
        .expect("the tick outlived the cache")
        .unwrap();
}

#[tokio::test]
async fn publish_sends_the_synced_list() {
    let (tx, mut rx) = broadcast::channel(4);
    let cache = StdMutex::new(RulesCache::default());
    lock(&cache).replace_all(vec![rule("a", "always", 0)]);
    publish_rules(&cache, &tx);
    match rx.try_recv().unwrap() {
        ServerMessage::SetRules { rules } => assert_eq!(rules[0]["name"], "a"),
        other => panic!("expected SetRules, got {other:?}"),
    }
}

// --- Issue #61 -------------------------------------------------------------

fn published(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<ServerMessage> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// Before the first sync (or after a withdrawal) a refused command's
/// re-publish still reaches the GUI, as an empty list, so a switch flipped
/// optimistically on a stale list is reset.
#[test]
fn publishing_while_unknown_sends_the_empty_list() {
    let (tx, mut rx) = broadcast::channel(8);
    let cache = StdMutex::new(RulesCache::default());
    publish_rules(&cache, &tx);
    let sent = published(&mut rx);
    assert!(
        sent.iter()
            .any(|m| matches!(m, ServerMessage::SetRules { rules } if rules.is_empty())),
        "{sent:?}"
    );
}

/// Rules Snitchwatch doesn't show are counted for the GUI, not only logged:
/// rules over the size limits, and a whole list over the rule limit.
#[test]
fn what_the_list_leaves_out_is_published_with_it() {
    let (tx, mut rx) = broadcast::channel(8);
    let sync = RulesSync::new(tx);
    lock(&sync.cache).over_limit_total = Some(MAX_SNAPSHOT_RULES + 1);
    sync.publish();
    let not_shown = |sent: Vec<ServerMessage>| {
        sent.into_iter().find_map(|m| match m {
            ServerMessage::RulesNotShown {
                too_large,
                over_limit_total,
                listed,
            } => Some((too_large, over_limit_total, listed)),
            _ => None,
        })
    };
    assert_eq!(
        not_shown(published(&mut rx)),
        Some((0, Some(MAX_SNAPSHOT_RULES as u32 + 1), false))
    );
    lock(&sync.cache).replace_all(vec![rule("a", "always", 0)]);
    lock(&sync.cache).set_left_out([("long".to_string(), 20_000)].into());
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((1, None, true)));
    // A count is shown only with no list, and an adopted list forgets it.
    lock(&sync.cache).over_limit_total = Some(MAX_SNAPSHOT_RULES + 1);
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((1, None, true)));
    lock(&sync.cache).replace_all(Vec::new());
    lock(&sync.cache).set_unknown();
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((0, None, false)));
    // A withdrawal takes the count with it, list or not.
    lock(&sync.cache).over_limit_total = Some(MAX_SNAPSHOT_RULES + 1);
    sync.withdraw();
    assert_eq!(not_shown(published(&mut rx)), Some((0, None, false)));
    assert_eq!(lock(&sync.cache).over_limit_total, None);
}

fn change(rule: Rule) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule],
        ..Default::default()
    }
}

fn changed(name: &str, duration: &str, enabled: bool) -> Rule {
    Rule {
        enabled,
        ..rule(name, duration, 0)
    }
}

/// PR #106 review M1/M2, the daemon's timers (`replaceUserRule`,
/// `scheduleTemporaryRule`): turning a rule off keeps its timer, which still
/// removes it; a new duration while off has none; turning it on starts one.
/// Each confirmed change is also the rule's new `created`.
#[test]
fn off_new_duration_on_expires_on_the_last_timer_and_stamps_each_change() {
    let mut cache = synced(vec![rule("a", "5m", T - 100)]);
    cache.apply_confirmed_at(&change(changed("a", "5m", false)), T);
    assert_eq!(get(&cache, "a").created, T);
    assert_eq!(cache.expiry_of("a"), Some(T + 200), "its timer still runs");
    cache.apply_confirmed_at(&change(changed("a", "1h", false)), T + 10);
    assert_eq!(cache.expiry_of("a"), None, "no timer of this duration");
    assert!(cache.prune_expired(T + 250).is_empty());
    cache.apply_confirmed_at(&change(changed("a", "1h", true)), T + 20);
    assert_eq!(get(&cache, "a").created, T + 20);
    assert!(cache.prune_expired(T + 20 + 3599).is_empty());
    assert_eq!(cache.prune_expired(T + 20 + 3600), vec!["a"]);
    assert_eq!(cache.expiry_of("a"), None);
}

/// A new enabled temporary rule (the rule editor's) expires on its own
/// timer; one added off has none.
#[test]
fn a_new_timed_rule_expires_only_when_it_is_on() {
    let mut cache = synced(Vec::new());
    cache.apply_confirmed_at(&change(changed("on", "5m", true)), T);
    cache.apply_confirmed_at(&change(changed("off", "5m", false)), T);
    assert_eq!(cache.prune_expired(T + 300), vec!["on"]);
    assert_eq!(names(&cache), vec!["off"]);
}

/// A timer that already fired is gone: a rule made again under the same
/// name (a prompt answered again) runs on a new one.
#[test]
fn a_rule_made_again_after_its_timer_fired_gets_a_new_one() {
    let mut cache = synced(vec![rule("a", "5m", T - 400)]);
    cache.upsert_at(rule("a", "5m", T), T);
    assert_eq!(cache.expiry_of("a"), Some(T + 300));
}

/// After a resync `created` says when the rule last changed, not whether a
/// timer runs: a rule listed off gets none from the list, so turning it on
/// starts one from then (a row left a little long is safer than an active
/// rule hidden early).
#[test]
fn a_rule_listed_off_has_no_timer_until_it_is_turned_on() {
    let mut off = rule("a", "5m", T - 240);
    off.enabled = false;
    let mut cache = synced(vec![off]);
    assert_eq!(cache.expiry_of("a"), None);
    cache.apply_confirmed_at(&change(changed("a", "5m", true)), T);
    assert_eq!(cache.expiry_of("a"), Some(T + 300));
}

/// The stage/commit race (#61): a commit adopts the newest snapshot staged
/// under its connection's key. On the Unix socket every connection is root's
/// daemon (one shared key), so the newest is the daemon's newest list; on
/// TCP each connection's key is its own peer address, so one connection
/// never commits another's snapshot.
#[test]
fn distinct_connections_never_take_each_others_snapshot() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), vec![rule("one", "always", 0)], now);
    pending.stage(key(2), vec![rule("two", "always", 0)], now);
    assert_eq!(
        pending.adopt_fresh(&key(1), 1, now).unwrap().rules[0].name,
        "one"
    );
    assert_eq!(
        pending.adopt_fresh(&key(2), 2, now).unwrap().rules[0].name,
        "two"
    );
}

/// PR #106 review H1: a remembered answer is announced (`UpdateRules`) only
/// when there is a list to add it to, and under the cache lock, so it can't
/// land in a GUI's empty list and vanish at the next `SetRules`, or race a
/// withdrawal.
#[test]
fn a_remembered_answer_is_announced_only_to_a_list() {
    let (tx, mut rx) = broadcast::channel(8);
    let sync = RulesSync::new(tx);
    sync.upsert(rule("snitchwatch-allow-a", "always", T));
    assert!(published(&mut rx).is_empty(), "announced with no list");
    lock(&sync.cache).replace_all(Vec::new());
    sync.upsert(rule("snitchwatch-allow-a", "always", T));
    let sent = published(&mut rx);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        matches!(&sent[0], ServerMessage::UpdateRules { rules }
            if rules.len() == 1 && rules[0]["name"] == "snitchwatch-allow-a"),
        "{sent:?}"
    );
}
