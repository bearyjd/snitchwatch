//! The bridge's copy of opensnitchd's rule list (issue #48).
//!
//! opensnitchd v1.8.0 has no rule push and no "list rules" action; its only
//! full snapshot is `ClientConfig.rules` on `Subscribe` (filled from
//! `c.rules.GetAll()`, temporary and disabled rules included). The bridge
//! stages that snapshot per connection ([`PendingSnapshots`]) and commits it
//! when the same connection's `Notifications` stream sends its HELLO
//! ([`RulesSync::commit`]): the daemon always subscribes *before* opening
//! the stream, and a dead old stream can linger until the HTTP/2 keepalive
//! notices, so a redial's snapshot waits for its own HELLO.
//!
//! The committed list belongs to the stream that committed it and is
//! withdrawn when that stream closes or stops being current (see
//! `daemon_commands`). Until then the cache follows the bridge's own
//! changes: remembered prompt verdicts (`ask_rule`) and rule commands the
//! daemon confirmed with `OK`, applied in reply order. Disk edits and
//! daemon-side expiry show up on the next (re)connect; expiry is
//! approximated by [`RulesCache::prune_expired`].
//!
//! Snapshots are bounded: more than [`MAX_SNAPSHOT_RULES`] rules are not
//! staged at all, and a rule with an over-long field is left out.
//!
//! Known divergence: the daemon stores a prompt reply through
//! `addUserRule` → `setUniqueName`, so it may hold `<name>-2` where the cache
//! holds `<name>`. #50's process-qualified names make this rare, and the next
//! `Subscribe` corrects it.

use crate::daemon_commands::{BecameCurrent, CommandError, ConnKey, PendingReply};
use crate::rule_wire::rule_to_wire;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

/// How many connections may have a staged snapshot at once. A unary
/// `Subscribe` has no close hook, so stale entries are bounded rather than
/// dropped on disconnect.
const PENDING_SNAPSHOT_CAP: usize = 4;
/// A staged snapshot older than this is never committed, and is evicted by
/// the next `stage` or commit.
const PENDING_SNAPSHOT_TTL: Duration = Duration::from_secs(30);
/// A larger `Subscribe` snapshot is not staged: a partial list would mislead.
pub const MAX_SNAPSHOT_RULES: usize = 10_000;
/// Longest accepted string field of a daemon rule (description, action,
/// duration, operator type/operand/data), in bytes.
pub const MAX_RULE_FIELD_BYTES: usize = 16 * 1024;
/// Most members a list operator may have, and how deeply lists may nest.
const MAX_OPERATOR_LIST_LEN: usize = 64;
const MAX_OPERATOR_DEPTH: usize = 4;

/// Shared handle to the cache. A std mutex: every operation is synchronous.
pub type SharedRulesCache = Arc<StdMutex<RulesCache>>;

/// `Unknown` means no daemon snapshot has been committed during this bridge
/// run, which is different from `Synced` with zero rules.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RulesCache {
    #[default]
    Unknown,
    Synced(BTreeMap<String, Rule>),
}

impl RulesCache {
    pub fn replace_all(&mut self, rules: Vec<Rule>) {
        *self = Self::Synced(rules.into_iter().map(|r| (r.name.clone(), r)).collect());
    }

    /// Insert or replace by name. When the rule is already cached and the
    /// incoming `created` is 0 (every GUI toggle goes through
    /// `rule_from_wire`, which zeroes it), the cached `created` is kept: the
    /// daemon's original expiry timer still fires on the original schedule.
    /// A no-op while `Unknown`: one rule is not the full list.
    pub fn upsert(&mut self, mut rule: Rule) {
        let Self::Synced(rules) = self else { return };
        if rule.created == 0 {
            if let Some(cached) = rules.get(&rule.name) {
                rule.created = cached.created;
            }
        }
        rules.insert(rule.name.clone(), rule);
    }

    /// A no-op while `Unknown`.
    pub fn remove(&mut self, name: &str) {
        if let Self::Synced(rules) = self {
            rules.remove(name);
        }
    }

    /// The full list in name order (the daemon evaluates enabled rules in
    /// `sort.Strings` order, `loader.go` `sortRules`), or `None` while
    /// `Unknown`.
    pub fn snapshot_wire(&self) -> Option<Vec<serde_json::Value>> {
        match self {
            Self::Unknown => None,
            Self::Synced(rules) => Some(rules.values().map(rule_to_wire).collect()),
        }
    }

    /// Drop temporary rules whose `created + duration` has passed. Returns
    /// whether anything was removed.
    pub fn prune_expired(&mut self, now_secs: i64) -> bool {
        let Self::Synced(rules) = self else {
            return false;
        };
        let before = rules.len();
        rules.retain(|_, rule| expires_at(rule).is_none_or(|at| at > now_secs));
        rules.len() != before
    }

    /// Apply a command the daemon answered `OK`: `CHANGE_RULE` upserts its
    /// rules, `DELETE_RULE` removes them by name.
    pub fn apply_confirmed(&mut self, sent: &Notification) {
        if sent.r#type == Action::ChangeRule as i32 {
            for rule in &sent.rules {
                self.upsert(rule.clone());
            }
        } else if sent.r#type == Action::DeleteRule as i32 {
            for rule in &sent.rules {
                self.remove(&rule.name);
            }
        }
    }
}

/// When a temporary rule (`loader.go` `isTemporary`: not `once`,
/// `until restart` or `always`) expires. Approximate: `created == 0` or a
/// duration that isn't a `\d+[smh]` sequence never expires.
fn expires_at(rule: &Rule) -> Option<i64> {
    if rule.created == 0 || matches!(rule.duration.as_str(), "once" | "until restart" | "always") {
        return None;
    }
    rule.created
        .checked_add(parse_duration_secs(&rule.duration)?)
}

fn parse_duration_secs(duration: &str) -> Option<i64> {
    let mut total: i64 = 0;
    let mut digits = String::new();
    for c in duration.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return None,
        };
        let value: i64 = std::mem::take(&mut digits).parse().ok()?;
        total = total.checked_add(value.checked_mul(unit)?)?;
    }
    (digits.is_empty() && !duration.is_empty()).then_some(total)
}

/// Per-connection `ClientConfig.rules` awaiting that connection's HELLO.
#[derive(Debug, Default)]
pub struct PendingSnapshots {
    entries: VecDeque<(ConnKey, Instant, Vec<Rule>)>,
}

impl PendingSnapshots {
    /// Keep only `key`'s latest snapshot, evicting stale entries and then the
    /// oldest key past the cap.
    pub fn stage(&mut self, key: ConnKey, rules: Vec<Rule>, now: Instant) {
        self.entries.retain(|(staged, at, _)| {
            *staged != key && now.saturating_duration_since(*at) <= PENDING_SNAPSHOT_TTL
        });
        self.entries.push_back((key, now, rules));
        while self.entries.len() > PENDING_SNAPSHOT_CAP {
            self.entries.pop_front();
        }
    }

    /// Remove `key`'s snapshot; `Some` only when it is fresh.
    pub fn take_fresh(&mut self, key: &ConnKey, now: Instant) -> Option<Vec<Rule>> {
        let index = self
            .entries
            .iter()
            .position(|(staged, _, _)| staged == key)?;
        let (_, staged_at, rules) = self.entries.remove(index)?;
        (now.saturating_duration_since(staged_at) <= PENDING_SNAPSHOT_TTL).then_some(rules)
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Broadcast the full list as `SetRules` when the cache is synced. Sent with
/// the cache lock held, so two publishers can't deliver lists out of order.
pub fn publish_rules(cache: &StdMutex<RulesCache>, broadcast: &broadcast::Sender<ServerMessage>) {
    let cache = lock(cache);
    if let Some(rules) = cache.snapshot_wire() {
        let _ = broadcast.send(ServerMessage::SetRules { rules });
    }
}

/// Whether a daemon rule fits the per-field limits.
fn within_limits(rule: &Rule) -> bool {
    fn operator_ok(op: &snitchwatch_proto::protocol::Operator, depth: usize) -> bool {
        depth <= MAX_OPERATOR_DEPTH
            && op.list.len() <= MAX_OPERATOR_LIST_LEN
            && [&op.r#type, &op.operand, &op.data]
                .iter()
                .all(|field| field.len() <= MAX_RULE_FIELD_BYTES)
            && op.list.iter().all(|member| operator_ok(member, depth + 1))
    }
    [&rule.name, &rule.description, &rule.action, &rule.duration]
        .iter()
        .all(|field| field.len() <= MAX_RULE_FIELD_BYTES)
        && rule.operator.as_ref().is_none_or(|op| operator_ok(op, 1))
}

/// The part of a `Subscribe` snapshot that may be staged: `None` when it has
/// too many rules, otherwise the rules within the field limits.
fn bounded_snapshot(rules: Vec<Rule>) -> Option<Vec<Rule>> {
    if rules.len() > MAX_SNAPSHOT_RULES {
        warn!(
            count = rules.len(),
            "daemon rule snapshot too large; not staged"
        );
        return None;
    }
    let total = rules.len();
    let kept: Vec<Rule> = rules.into_iter().filter(within_limits).collect();
    if kept.len() != total {
        warn!(
            dropped = total - kept.len(),
            "left out daemon rules over the size limits"
        );
    }
    Some(kept)
}

/// `UiService`'s rule state: the cache, the staged snapshots, a generation
/// bumped on every commit, and the broadcast `SetRules` goes out on.
#[derive(Clone)]
pub struct RulesSync {
    cache: SharedRulesCache,
    pending: Arc<StdMutex<PendingSnapshots>>,
    synced: Arc<watch::Sender<u64>>,
    broadcast: broadcast::Sender<ServerMessage>,
}

impl RulesSync {
    pub fn new(broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self {
            cache: SharedRulesCache::default(),
            pending: Arc::default(),
            synced: Arc::new(watch::channel(0).0),
            broadcast,
        }
    }

    pub fn cache(&self) -> SharedRulesCache {
        self.cache.clone()
    }

    /// Generation bumped each time a daemon snapshot is committed.
    pub fn synced(&self) -> watch::Receiver<u64> {
        self.synced.subscribe()
    }

    /// A remembered prompt verdict (see [`RulesCache::upsert`]).
    pub fn upsert(&self, rule: Rule) {
        lock(&self.cache).upsert(rule);
    }

    /// Hold a `Subscribe`'s rules until its connection sends HELLO. An
    /// oversized snapshot also discards the key's earlier one.
    pub fn stage(&self, key: ConnKey, rules: Vec<Rule>) {
        let now = Instant::now();
        let mut pending = lock(&self.pending);
        match bounded_snapshot(rules) {
            Some(rules) => pending.stage(key, rules, now),
            None => drop(pending.take_fresh(&key, now)),
        }
    }

    /// Adopt the connection's fresh staged snapshot, if any, and broadcast
    /// it. `current` proves the HELLO's stream is current and the stream
    /// lock is held. Returns whether a snapshot was committed.
    pub(crate) fn commit(&self, current: &BecameCurrent<'_>) -> bool {
        let staged = lock(&self.pending).take_fresh(&current.conn(), Instant::now());
        let Some(rules) = staged else {
            return false;
        };
        info!(
            stream = current.stream(),
            rules = rules.len(),
            "adopted the daemon's rule snapshot"
        );
        lock(&self.cache).replace_all(rules);
        self.publish();
        self.synced.send_modify(|generation| *generation += 1);
        true
    }

    /// The stream the list came from is gone or no longer current: forget
    /// the list and clear it in every GUI, so none of its rows can be acted
    /// on under another stream.
    pub(crate) fn withdraw(&self) {
        let mut cache = lock(&self.cache);
        if *cache == RulesCache::Unknown {
            return;
        }
        *cache = RulesCache::Unknown;
        info!("withdrew the daemon rule list; its stream is gone");
        let _ = self
            .broadcast
            .send(ServerMessage::SetRules { rules: Vec::new() });
    }

    /// A command the daemon answered `OK`, in reply order.
    pub(crate) fn apply_confirmed(&self, sent: &Notification) {
        lock(&self.cache).apply_confirmed(sent);
        self.publish();
    }

    pub fn publish(&self) {
        publish_rules(&self.cache, &self.broadcast);
    }
}

/// Wait for the daemon's answer to a rule command. An `OK` was already
/// applied to the cache and broadcast by `DaemonCommands::on_reply`; any
/// other outcome re-broadcasts the unchanged list, so a GUI that flipped a
/// switch optimistically is corrected.
pub async fn settle_rule_command(
    pending: PendingReply,
    cache: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
    timeout: Duration,
) {
    let id = pending.id();
    match pending.wait(timeout).await {
        Ok(()) => return,
        Err(CommandError::Rejected(reason)) => {
            warn!(id, ?reason, "daemon rejected rule command")
        }
        Err(error) => warn!(id, ?error, "rule command not confirmed by the daemon"),
    }
    publish_rules(&cache, &broadcast);
}

/// Every `period`, prune expired temporary rules and broadcast the list if
/// anything went. Ends once the cache itself is gone (bridge shut down).
pub async fn prune_expired_rules_every(
    period: Duration,
    cache: Weak<StdMutex<RulesCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
) {
    let mut ticks = tokio::time::interval(period);
    loop {
        ticks.tick().await;
        let Some(cache) = cache.upgrade() else {
            return;
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if lock(&cache).prune_expired(now_secs) {
            publish_rules(&cache, &broadcast);
        }
    }
}

#[cfg(test)]
mod tests {
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
        let mut cache = RulesCache::Unknown;
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
        match cache {
            RulesCache::Synced(rules) => &rules[name],
            RulesCache::Unknown => panic!("cache is Unknown"),
        }
    }

    #[test]
    fn unknown_yields_no_snapshot_and_synced_empty_yields_an_empty_one() {
        assert_eq!(RulesCache::Unknown.snapshot_wire(), None);
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
        let mut cache = RulesCache::Unknown;
        cache.upsert(rule("a", "always", 0));
        cache.remove("a");
        assert_eq!(cache, RulesCache::Unknown);
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
        assert!(cache.prune_expired(T));
        assert_eq!(names(&cache), vec!["b"]);
        assert!(!cache.prune_expired(T), "nothing left to prune");
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
        assert!(!cache.prune_expired(T));
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

        assert!(!cache.prune_expired(T + 31));
        assert!(!get(&cache, "a").enabled);
        assert_eq!(get(&cache, "a").created, T);

        assert!(cache.prune_expired(T + 301));
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
        assert_eq!(taken[0].name, "new");
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
        let names: Vec<_> = kept.iter().map(|r| r.name.as_str()).collect();
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
        assert_eq!(pending.take_fresh(&None, now).unwrap()[0].name, "b");
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
            tx,
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
    async fn publish_sends_set_rules_only_when_synced() {
        let (tx, mut rx) = broadcast::channel(4);
        let cache = StdMutex::new(RulesCache::Unknown);
        publish_rules(&cache, &tx);
        assert!(rx.try_recv().is_err());

        lock(&cache).replace_all(vec![rule("a", "always", 0)]);
        publish_rules(&cache, &tx);
        match rx.try_recv().unwrap() {
            ServerMessage::SetRules { rules } => assert_eq!(rules[0]["name"], "a"),
            other => panic!("expected SetRules, got {other:?}"),
        }
    }
}
