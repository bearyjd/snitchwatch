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

use crate::cache::rule_hits_handle::RuleHitsHandle;
use crate::daemon_commands::{BecameCurrent, CommandError, ConnKey, PendingReply};
use crate::rule_wire::rule_to_wire;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
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
pub(crate) const MAX_OPERATOR_LIST_LEN: usize = 64;
pub(crate) const MAX_OPERATOR_DEPTH: usize = 4;

/// Shared handle to the cache. A std mutex: every operation is synchronous.
pub type SharedRulesCache = Arc<StdMutex<RulesCache>>;

/// The bridge's copy of the daemon's rules by name: `None` ("Unknown") until
/// a daemon snapshot is committed during this bridge run, which is different
/// from an empty list. `revision` is bumped by every change and never reset
/// (rule import's stale-preview check, roadmap P2.7); it lives here, not in
/// [`RulesSync`], because [`prune_expired_rules_every`] prunes directly.
/// `left_out` holds the daemon rules the snapshot left out for the size
/// limits: their names (an import must not overwrite one unseen) and
/// protobuf sizes (the daemon still sends them in every snapshot).
#[derive(Debug, Clone, Default)]
pub struct RulesCache {
    rules: Option<BTreeMap<String, Rule>>,
    revision: u64,
    left_out: BTreeMap<String, usize>,
}

/// A `Subscribe` snapshot within the limits, and what it left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) rules: Vec<Rule>,
    /// Name (or, for a name no import could use, a NUL-prefixed index) to
    /// encoded size.
    pub(crate) left_out: BTreeMap<String, usize>,
}

impl From<Vec<Rule>> for Snapshot {
    fn from(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            left_out: BTreeMap::new(),
        }
    }
}

impl RulesCache {
    pub fn rules(&self) -> Option<&BTreeMap<String, Rule>> {
        self.rules.as_ref()
    }

    pub fn is_unknown(&self) -> bool {
        self.rules.is_none()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Daemon rules left out of the list for the size limits.
    pub fn left_out(&self) -> &BTreeMap<String, usize> {
        &self.left_out
    }

    pub(crate) fn set_left_out(&mut self, left_out: BTreeMap<String, usize>) {
        self.left_out = left_out;
    }

    pub fn replace_all(&mut self, rules: Vec<Rule>) {
        self.rules = Some(rules.into_iter().map(|r| (r.name.clone(), r)).collect());
        self.revision += 1;
    }

    /// Forget the list (its stream is gone). A no-op while `Unknown`.
    pub fn set_unknown(&mut self) {
        self.left_out.clear();
        if self.rules.take().is_some() {
            self.revision += 1;
        }
    }

    /// Insert or replace by name. When the rule is already cached and the
    /// incoming `created` is 0 (every GUI toggle goes through
    /// `rule_from_wire`, which zeroes it), the cached `created` is kept: the
    /// daemon's original expiry timer still fires on the original schedule.
    /// A no-op while `Unknown`: one rule is not the full list.
    pub fn upsert(&mut self, mut rule: Rule) {
        let Some(rules) = &mut self.rules else { return };
        if rule.created == 0 {
            if let Some(cached) = rules.get(&rule.name) {
                rule.created = cached.created;
            }
        }
        self.left_out.remove(&rule.name);
        rules.insert(rule.name.clone(), rule);
        self.revision += 1;
    }

    /// Whether the list is synced and has a rule of this name.
    pub fn contains(&self, name: &str) -> bool {
        self.rules().is_some_and(|rules| rules.contains_key(name))
    }

    /// A no-op while `Unknown`.
    pub fn remove(&mut self, name: &str) {
        let removed = self.rules.as_mut().and_then(|rules| rules.remove(name));
        let was_left_out = self.rules.is_some() && self.left_out.remove(name).is_some();
        if removed.is_some() || was_left_out {
            self.revision += 1;
        }
    }

    /// The full list in name order (the daemon evaluates enabled rules in
    /// `sort.Strings` order, `loader.go` `sortRules`), or `None` while
    /// `Unknown`.
    pub fn snapshot_wire(&self) -> Option<Vec<serde_json::Value>> {
        self.rules()
            .map(|rules| rules.values().map(rule_to_wire).collect())
    }

    /// Drop temporary rules whose `created + duration` has passed. Returns
    /// whether anything was removed.
    pub fn prune_expired(&mut self, now_secs: i64) -> bool {
        let Some(rules) = &mut self.rules else {
            return false;
        };
        let before = rules.len();
        rules.retain(|_, rule| expires_at(rule).is_none_or(|at| at > now_secs));
        let removed = rules.len() != before;
        self.revision += u64::from(removed);
        removed
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
    entries: VecDeque<(ConnKey, Instant, Snapshot)>,
}

impl PendingSnapshots {
    /// Keep only `key`'s latest snapshot, evicting stale entries and then the
    /// oldest key past the cap.
    pub(crate) fn stage(&mut self, key: ConnKey, rules: impl Into<Snapshot>, now: Instant) {
        self.entries.retain(|(staged, at, _)| {
            *staged != key && now.saturating_duration_since(*at) <= PENDING_SNAPSHOT_TTL
        });
        self.entries.push_back((key, now, rules.into()));
        while self.entries.len() > PENDING_SNAPSHOT_CAP {
            self.entries.pop_front();
        }
    }

    /// Remove `key`'s snapshot; `Some` only when it is fresh.
    pub(crate) fn take_fresh(&mut self, key: &ConnKey, now: Instant) -> Option<Snapshot> {
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

/// Whether a daemon rule fits the per-field limits. Imported rules use the
/// same check (`rule_policy::validate_user_rule`), so an import can't install
/// a rule the next snapshot would leave out.
pub(crate) fn within_limits(rule: &Rule) -> bool {
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
/// too many rules, otherwise the rules within the field limits and what was
/// left out.
fn bounded_snapshot(rules: Vec<Rule>) -> Option<Snapshot> {
    if rules.len() > MAX_SNAPSHOT_RULES {
        warn!(
            count = rules.len(),
            "daemon rule snapshot too large; not staged"
        );
        return None;
    }
    let mut snapshot = Snapshot::default();
    for (index, rule) in rules.into_iter().enumerate() {
        if within_limits(&rule) {
            snapshot.rules.push(rule);
            continue;
        }
        let key = if rule.name.len() <= crate::rule_name::MAX_RULE_NAME_BYTES {
            rule.name.clone()
        } else {
            format!("\0{index}")
        };
        *snapshot.left_out.entry(key).or_default() += prost::Message::encoded_len(&rule);
    }
    if !snapshot.left_out.is_empty() {
        warn!(
            dropped = snapshot.left_out.len(),
            "left out daemon rules over the size limits"
        );
    }
    Some(snapshot)
}

/// `UiService`'s rule state: the cache, the staged snapshots, a generation
/// bumped on every commit, the broadcast `SetRules` goes out on, and the
/// per-rule hit counts that follow the list (a committed snapshot and a
/// confirmed `DELETE_RULE` prune them; [`Self::withdraw`] never does).
#[derive(Clone)]
pub struct RulesSync {
    cache: SharedRulesCache,
    pending: Arc<StdMutex<PendingSnapshots>>,
    synced: Arc<watch::Sender<u64>>,
    broadcast: broadcast::Sender<ServerMessage>,
    hits: RuleHitsHandle,
    /// Live [`PublishHold`]s; see [`RulesSync::hold_publishes`].
    holds: Arc<AtomicUsize>,
}

/// While any is alive, confirmed commands update the cache without
/// broadcasting `SetRules`; dropping the last one broadcasts the list once.
pub struct PublishHold(RulesSync);

impl Drop for PublishHold {
    fn drop(&mut self) {
        if self.0.holds.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.publish();
        }
    }
}

impl RulesSync {
    pub fn new(broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self {
            cache: SharedRulesCache::default(),
            pending: Arc::default(),
            synced: Arc::new(watch::channel(0).0),
            hits: RuleHitsHandle::new(broadcast.clone()),
            broadcast,
            holds: Arc::default(),
        }
    }

    pub fn cache(&self) -> SharedRulesCache {
        self.cache.clone()
    }

    /// Per-rule hit counts (`cache::rule_hits`).
    pub fn hits(&self) -> RuleHitsHandle {
        self.hits.clone()
    }

    /// Counts the events of a ping that carried statistics.
    pub fn record_hits(&self, events: &[snitchwatch_proto::protocol::Event], uptime: u64) {
        self.hits.record(events, uptime, &self.cache);
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
            Some(snapshot) => pending.stage(key, snapshot, now),
            None => drop(pending.take_fresh(&key, now)),
        }
    }

    /// Adopt the connection's fresh staged snapshot, if any, and broadcast
    /// it. `current` proves the HELLO's stream is current and the stream
    /// lock is held. Returns whether a snapshot was committed.
    pub(crate) fn commit(&self, current: &BecameCurrent<'_>) -> bool {
        let staged = lock(&self.pending).take_fresh(&current.conn(), Instant::now());
        let Some(snapshot) = staged else {
            return false;
        };
        info!(
            stream = current.stream(),
            rules = snapshot.rules.len(),
            "adopted the daemon's rule snapshot"
        );
        {
            let mut cache = lock(&self.cache);
            cache.replace_all(snapshot.rules);
            cache.set_left_out(snapshot.left_out);
            self.hits.adopt_snapshot(&cache);
        }
        self.publish();
        self.synced.send_modify(|generation| *generation += 1);
        true
    }

    /// The stream the list came from is gone or no longer current: forget
    /// the list and clear it in every GUI, so none of its rows can be acted
    /// on under another stream.
    pub(crate) fn withdraw(&self) {
        let mut cache = lock(&self.cache);
        if cache.is_unknown() {
            return;
        }
        cache.set_unknown();
        info!("withdrew the daemon rule list; its stream is gone");
        let _ = self
            .broadcast
            .send(ServerMessage::SetRules { rules: Vec::new() });
    }

    /// A command the daemon answered `OK`, in reply order.
    pub(crate) fn apply_confirmed(&self, sent: &Notification) {
        {
            let mut cache = lock(&self.cache);
            cache.apply_confirmed(sent);
            if sent.r#type == Action::DeleteRule as i32 {
                self.hits
                    .forget(sent.rules.iter().map(|rule| rule.name.as_str()));
            }
        }
        if self.holds.load(Ordering::SeqCst) == 0 {
            self.publish();
        }
    }

    /// Coalesce the `SetRules` broadcasts of confirmed commands until the
    /// returned hold drops (a rule import confirms one rule per reply; a full
    /// list per reply would cost O(n²) serialization in every GUI).
    pub fn hold_publishes(&self) -> PublishHold {
        self.holds.fetch_add(1, Ordering::SeqCst);
        PublishHold(self.clone())
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
#[path = "rules_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rules_revision_tests.rs"]
mod revision_tests;
