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

#[test]
fn upsert_keeps_the_cached_created_only_when_the_incoming_one_is_zero() {
    let mut cache = synced(vec![rule("a", "5m", T)]);
    cache.upsert(rule("a", "5m", 0));
    assert_eq!(get(&cache, "a").created, T);
    cache.upsert(rule("a", "5m", T + 10));
    assert_eq!(get(&cache, "a").created, T + 10);
    cache.upsert(rule("new", "always", 0));
    assert_eq!(names(&cache), vec!["a", "new"]);
    cache.remove("a");
    assert_eq!(names(&cache), vec!["new"]);
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
/// original schedule (`scheduleTemporaryRule`), so the cache must too.
#[test]
fn a_toggled_temporary_rule_keeps_its_original_expiry() {
    let mut cache = synced(vec![rule("a", "5m", T)]);
    let mut toggled = rule_from_wire(&rule_to_wire(get(&cache, "a"))).unwrap();
    assert_eq!(toggled.created, 0, "rule_from_wire zeroes created");
    toggled.enabled = false;
    cache.upsert(toggled);

    assert!(cache.prune_expired(T + 31).is_empty());
    assert!(!get(&cache, "a").enabled);
    assert_eq!(get(&cache, "a").created, T);

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
    cache.apply_confirmed(&Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![toggled],
        ..Default::default()
    });
    assert!(!get(&cache, "a").enabled);
    assert_eq!(get(&cache, "a").created, T);

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

#[test]
fn a_key_keeps_only_its_latest_snapshot_and_a_commit_removes_it() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), vec![rule("old", "always", 0)], now);
    pending.stage(key(1), vec![rule("new", "always", 0)], now);

    let taken = pending.take_fresh(&key(1), now).unwrap();
    assert_eq!(taken.rules[0].name, "new");
    assert_eq!(pending.take_fresh(&key(1), now), None, "taken once");
}

#[test]
fn a_fifth_key_evicts_the_oldest() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    for port in 1..=5 {
        pending.stage(key(port), Vec::new(), now);
    }
    assert_eq!(pending.take_fresh(&key(1), now), None);
    for port in 2..=5 {
        assert!(pending.take_fresh(&key(port), now).is_some(), "{port}");
    }
}

#[test]
fn a_stale_snapshot_is_not_committed_and_is_removed() {
    let then = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(key(1), Vec::new(), then);

    assert_eq!(
        pending.take_fresh(&key(1), then + Duration::from_secs(31)),
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
    assert_eq!(pending.entries[0].0, key(2));
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
    assert_eq!(bounded_snapshot(too_many), None);

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
    ])
    .unwrap();
    let names: Vec<_> = kept.rules.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["ok", "deepest-allowed"]);
}

#[test]
fn an_oversized_snapshot_discards_the_connections_earlier_one() {
    let sync = RulesSync::new(broadcast::channel(4).0);
    sync.stage(key(1), vec![rule("a", "always", 0)]);
    sync.stage(key(1), vec![rule("a", "always", 0); MAX_SNAPSHOT_RULES + 1]);
    assert!(lock(&sync.pending).entries.is_empty());
}

#[test]
fn the_none_key_is_one_shared_key() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    pending.stage(None, vec![rule("a", "always", 0)], now);
    pending.stage(None, vec![rule("b", "always", 0)], now);
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(pending.take_fresh(&None, now).unwrap().rules[0].name, "b");
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

    match rx.recv().await.unwrap() {
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
    sync.stage(None, vec![rule("a", "always", 0); MAX_SNAPSHOT_RULES + 1]);
    sync.publish();
    let not_shown = |sent: Vec<ServerMessage>| {
        sent.into_iter().find_map(|m| match m {
            ServerMessage::RulesNotShown {
                too_large,
                over_limit_total,
            } => Some((too_large, over_limit_total)),
            _ => None,
        })
    };
    assert_eq!(
        not_shown(published(&mut rx)),
        Some((0, Some(MAX_SNAPSHOT_RULES as u32 + 1)))
    );
    lock(&sync.cache).replace_all(vec![rule("a", "always", 0)]);
    lock(&sync.cache).set_left_out([("long".to_string(), 20_000)].into());
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((1, None)));
    // A list is shown: an oversized snapshot staged since isn't counted,
    // and an adopted list forgets the earlier one.
    sync.stage(None, vec![rule("a", "always", 0); MAX_SNAPSHOT_RULES + 1]);
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((1, None)));
    lock(&sync.cache).replace_all(Vec::new());
    lock(&sync.cache).set_unknown();
    sync.publish();
    assert_eq!(not_shown(published(&mut rx)), Some((0, None)));
}

/// A rule whose duration changed has a new clock (`scheduleTemporaryRule`),
/// so the old `created` doesn't carry over: the cache must not prune a rule
/// edited from 5 minutes to an hour after the first 5.
#[test]
fn a_changed_duration_does_not_keep_the_old_clock() {
    let mut cache = synced(vec![rule("a", "5m", T)]);
    cache.upsert(rule("a", "1h", 0));
    let created = get(&cache, "a").created;
    assert_ne!(created, T, "the old clock carried over");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now - created).abs() < 5, "a new clock: {created}");
    let mut off = rule("b", "1h", 0);
    off.enabled = false;
    let mut cache = synced(vec![rule("b", "5m", T)]);
    cache.upsert(off);
    assert_eq!(get(&cache, "b").created, 0, "a disabled rule has no clock");
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
        pending.take_fresh(&key(1), now).unwrap().rules[0].name,
        "one"
    );
    assert_eq!(
        pending.take_fresh(&key(2), now).unwrap().rules[0].name,
        "two"
    );
}
