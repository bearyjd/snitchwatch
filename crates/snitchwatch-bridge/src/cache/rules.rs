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

use crate::accounts::{look_up_blocking, user_name_uids, AccountLookup, KnownAccounts};
use crate::cache::rule_hits_handle::RuleHitsHandle;
use crate::daemon_commands::{BecameCurrent, CommandError, ConnKey, PendingReply, StreamId};
use crate::rule_wire::rule_to_wire;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
/// Longest a `Subscribe` waits for account names (`RulesSync::learn_account_names`).
const ACCOUNT_LOOKUP_WAIT: Duration = Duration::from_secs(1);
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
    /// How many rules the daemon's last snapshot had when it was over
    /// [`MAX_SNAPSHOT_RULES`] and so not read at all (issue #61).
    over_limit_total: Option<usize>,
    /// When the daemon's timer removes each temporary rule; see [`Expiry`].
    expiries: BTreeMap<String, Expiry>,
    /// Account names for `user.name` uids, sent with the rules for display
    /// (`accounts`, PR #106 review M4). Kept across lists.
    accounts: KnownAccounts,
    /// Names whose `DELETE_RULE` the daemon refused: see
    /// [`Self::files_left`] (`rules_refused.rs`). Kept across lists.
    files_left: BTreeSet<String>,
    /// The debounce behind the Rules page's "different number of rules"
    /// hint (`rules_count.rs`). Not part of the list: no revision bump.
    count_watch: count::CountWatch,
    /// A rule left out for the size limits was temporary, so its daemon
    /// timer may drop it unseen (`rules_count.rs`).
    left_out_temporary: bool,
    /// Rules the daemon may hold that the list does not show
    /// (`rules_count.rs`). Cleared with every list.
    may_hold: count::MayHold,
}

/// The daemon timer that will remove a temporary rule (PR #106 review M2),
/// kept apart from `created`, which is the daemon's own stamp of the rule's
/// last change. `replaceUserRule` starts a timer each time it stores the
/// rule enabled; when one fires, `scheduleTemporaryRule` removes the rule
/// if its duration is still the one the timer was set for (turned off or
/// not), and does nothing otherwise. So the first timer to fire with the
/// rule's current duration removes it. Only that one is kept: a timer of
/// another duration is forgotten, which can only keep a row listed longer
/// than the daemon keeps the rule, never hide an active rule early.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Expiry {
    duration: String,
    at: i64,
    /// When the daemon's timer fires on its own clock, the monotonic one
    /// (Go's runtime timers; `CLOCK_MONOTONIC` stops while the host is
    /// suspended, the wall clock `at` does not). See `rules_count.rs`.
    ends: Instant,
}

/// A `Subscribe` snapshot within the limits, and what it left out; or, for
/// one over [`MAX_SNAPSHOT_RULES`], only how many rules it had.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) rules: Vec<Rule>,
    /// Name (or, for a name no import could use, a NUL-prefixed index) to
    /// encoded size.
    pub(crate) left_out: BTreeMap<String, usize>,
    /// Set when the snapshot was over [`MAX_SNAPSHOT_RULES`]: none of its
    /// rules were kept. Staged and adopted like a list, so the count shown
    /// belongs to the connection that sent it (PR #106 review L1).
    pub(crate) over_limit: Option<usize>,
    /// A rule left out was temporary (`rules_count.rs`).
    pub(crate) left_out_temporary: bool,
}

impl From<Vec<Rule>> for Snapshot {
    fn from(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }
}

/// What a HELLO found staged for its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Adopted {
    /// A list, now the cache's.
    List,
    /// A snapshot over the rule limit: no list, and its count shown.
    OverLimit,
    /// Nothing fresh that this stream hasn't adopted already.
    Nothing,
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

    /// Set when a snapshot is committed; public so other crates' tests can
    /// stand in for an oversized daemon rule.
    pub fn set_left_out(&mut self, left_out: BTreeMap<String, usize>) {
        for name in left_out.keys() {
            self.forget_file_left(name);
        }
        self.left_out = left_out;
    }

    /// Adopt a daemon snapshot. An enabled temporary rule's timer is taken
    /// to have started at its `created` (the daemon's stamp of its last
    /// change, when the timer started); a disabled one is given none, since
    /// whether one still runs isn't known.
    pub fn replace_all(&mut self, rules: Vec<Rule>) {
        let (now, clock) = (now_secs(), Instant::now());
        self.expiries = rules
            .iter()
            .filter(|rule| rule.enabled && rule.created > 0)
            .filter_map(|rule| {
                Some((
                    rule.name.clone(),
                    timer_from(rule, rule.created, now, clock)?,
                ))
            })
            .collect();
        for rule in &rules {
            // Listed again: the daemon loaded its file.
            self.files_left.remove(&rule.name);
        }
        self.rules = Some(rules.into_iter().map(|r| (r.name.clone(), r)).collect());
        self.over_limit_total = None;
        self.count_watch = count::CountWatch::default();
        self.left_out_temporary = false;
        self.may_hold.clear();
        self.revision += 1;
    }

    /// When the daemon's timer will remove `name`, if one will.
    pub fn expiry_of(&self, name: &str) -> Option<i64> {
        self.expiries.get(name).map(|expiry| expiry.at)
    }

    /// What the GUI's list leaves out (issue #61): rules over the size
    /// limits, and, while there is no list, a snapshot over the rule limit.
    fn not_shown(&self) -> ServerMessage {
        ServerMessage::RulesNotShown {
            too_large: u32::try_from(self.left_out.len()).unwrap_or(u32::MAX),
            over_limit_total: self
                .over_limit_total
                .filter(|_| self.is_unknown())
                .map(|total| u32::try_from(total).unwrap_or(u32::MAX)),
            listed: !self.is_unknown(),
            left_on_disk: u32::try_from(self.files_left.len()).unwrap_or(u32::MAX),
            count_mismatch: self.count_mismatch(),
        }
    }

    /// Forget the list (its stream is gone). A no-op while `Unknown`.
    pub fn set_unknown(&mut self) {
        self.left_out.clear();
        self.expiries.clear();
        self.count_watch = count::CountWatch::default();
        self.left_out_temporary = false;
        self.may_hold.clear();
        if self.rules.take().is_some() {
            self.revision += 1;
        }
    }

    /// Insert or replace by name, now. See [`Self::upsert_at`].
    pub fn upsert(&mut self, rule: Rule) {
        self.upsert_at(rule, now_secs());
    }

    /// Insert or replace by name, as the daemon stores a change at
    /// `now_secs` (PR #106 review M2). `created` is the daemon's stamp,
    /// which it makes anew on every change (`rule.Create` from each
    /// `CHANGE_RULE`): a rule without one is stamped `now_secs`, and one
    /// with one keeps it, which only a prompt answer the bridge stamped
    /// itself does ([`Self::apply_confirmed_at`] restamps every change). Its
    /// [`Expiry`]: a running timer of the same duration still removes it
    /// first; otherwise an enabled temporary rule starts one now, and any
    /// other has none. A no-op while `Unknown`: one rule is not the full
    /// list.
    pub fn upsert_at(&mut self, mut rule: Rule, now_secs: i64) {
        let Some(rules) = &mut self.rules else { return };
        if rule.created == 0 {
            rule.created = now_secs;
        }
        let running = self
            .expiries
            .remove(&rule.name)
            .filter(|kept| kept.duration == rule.duration && kept.at > now_secs);
        let timer = running.or_else(|| {
            rule.enabled
                .then(|| timer_from(&rule, now_secs, now_secs, Instant::now()))
                .flatten()
        });
        if let Some(timer) = timer {
            self.expiries.insert(rule.name.clone(), timer);
        }
        self.left_out.remove(&rule.name);
        self.may_hold.forget(&rule.name);
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
        self.expiries.remove(name);
        self.may_hold.forget(name);
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
            .map(|rules| rules.values().map(|rule| self.wire(rule)).collect())
    }

    /// A rule's wire shape, with the account names its `user.name` uids
    /// have (`userNames`, display only) when any are known.
    fn wire(&self, rule: &Rule) -> serde_json::Value {
        let mut wire = rule_to_wire(rule);
        let names = self.accounts.names_for(rule);
        if !names.is_empty() {
            wire["userNames"] = serde_json::json!(names);
        }
        wire
    }

    /// Drop temporary rules whose daemon timer ([`Expiry`]) has fired.
    /// Returns the names removed, in name order.
    pub fn prune_expired(&mut self, now_secs: i64) -> Vec<String> {
        self.prune_expired_at(now_secs, Instant::now())
    }

    /// [`prune_expired`](Self::prune_expired) at a given wall time and
    /// monotonic reading. A rule pruned while its daemon timer, which runs on
    /// the monotonic clock, has not fired yet (the host was suspended) may
    /// still be in the daemon: see `rules_count.rs` `MayHold`.
    pub fn prune_expired_at(&mut self, now_secs: i64, clock: Instant) -> Vec<String> {
        let Some(rules) = &mut self.rules else {
            return Vec::new();
        };
        let expired: Vec<String> = self
            .expiries
            .iter()
            .filter(|(_, expiry)| expiry.at <= now_secs)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &expired {
            rules.remove(name);
            if let Some(expiry) = self.expiries.remove(name) {
                self.may_hold.note_pruned(expiry.ends, clock);
            }
        }
        self.revision += u64::from(!expired.is_empty());
        expired
    }

    /// Apply a command the daemon answered `OK`: `CHANGE_RULE` upserts its
    /// rules (an `always` one's file was written: [`Self::files_left`]
    /// forgets it), `DELETE_RULE` removes them by name.
    pub fn apply_confirmed(&mut self, sent: &Notification) {
        self.apply_confirmed_at(sent, now_secs());
    }

    /// [`apply_confirmed`](Self::apply_confirmed) at a given time.
    pub fn apply_confirmed_at(&mut self, sent: &Notification, now_secs: i64) {
        if sent.r#type == Action::ChangeRule as i32 {
            for rule in &sent.rules {
                if rule.duration == "always" {
                    self.forget_file_left(&rule.name);
                }
                // The daemon makes the rule anew and stamps it, whatever
                // stamp it carried (`Deserialize`, `rule.Create`; N5).
                let restamped = Rule {
                    created: now_secs,
                    ..rule.clone()
                };
                self.upsert_at(restamped, now_secs);
            }
        } else if sent.r#type == Action::DeleteRule as i32 {
            for rule in &sent.rules {
                self.remove(&rule.name);
            }
        }
    }
}

/// The time of day in Unix seconds: `i64::MAX` if it does not fit, 0 for a
/// clock before 1970.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The timer the daemon starts for `rule` at `start`, if it is temporary
/// (`loader.go` `isTemporary`: not `once`, `until restart` or `always`).
/// Approximate: a duration that isn't a `\d+[smh]` sequence has none.
fn timer_from(rule: &Rule, start: i64, now_secs: i64, clock: Instant) -> Option<Expiry> {
    if !count::is_temporary(&rule.duration) {
        return None;
    }
    let at = start.checked_add(parse_duration_secs(&rule.duration)?)?;
    let remaining = Duration::from_secs(u64::try_from(at.saturating_sub(now_secs)).unwrap_or(0));
    Some(Expiry {
        duration: rule.duration.clone(),
        at,
        ends: clock.checked_add(remaining).unwrap_or(clock),
    })
}

pub(crate) fn parse_duration_secs(duration: &str) -> Option<i64> {
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
/// A snapshot stays staged after a stream adopts it, until the next one for
/// its key or [`PENDING_SNAPSHOT_TTL`]: another stream of the same key may
/// still say HELLO (PR #106 review OQ1). On the Unix socket every stream
/// shares one key, so a redialled daemon's old and new streams may both say
/// HELLO after the new one subscribed, in either order. Each adopts the new
/// stream's snapshot once, and when the stream holding the list closes, the
/// stream that falls back to current holds it if it adopted the same
/// snapshot ([`RulesSync::readopt`], review N1). Only within the 30 s: a
/// stream that becomes current later finds nothing, and the list is
/// withdrawn until the daemon reconnects. On TCP each connection has its own
/// key, and a closed stream's list is never handed to another. The cost: adopting it again replaces the list, so a change confirmed
/// on the old stream between the two adoptions drops off the list (and its
/// hit count restarts) until the daemon reconnects.
#[derive(Debug, Default)]
pub struct PendingSnapshots {
    entries: VecDeque<Staged>,
}

#[derive(Debug)]
struct Staged {
    key: ConnKey,
    at: Instant,
    snapshot: Snapshot,
    adopted_by: Vec<StreamId>,
}

impl PendingSnapshots {
    /// Keep only `key`'s latest snapshot, evicting stale entries and then the
    /// oldest key past the cap.
    pub(crate) fn stage(&mut self, key: ConnKey, rules: impl Into<Snapshot>, now: Instant) {
        self.entries.retain(|staged| {
            staged.key != key && now.saturating_duration_since(staged.at) <= PENDING_SNAPSHOT_TTL
        });
        self.entries.push_back(Staged {
            key,
            at: now,
            snapshot: rules.into(),
            adopted_by: Vec::new(),
        });
        while self.entries.len() > PENDING_SNAPSHOT_CAP {
            self.entries.pop_front();
        }
    }

    /// Whether a fresh snapshot waits for its stream's HELLO.
    pub(crate) fn awaiting_adoption(&self, now: Instant) -> bool {
        self.entries.iter().any(|staged| {
            staged.adopted_by.is_empty()
                && now.saturating_duration_since(staged.at) <= PENDING_SNAPSHOT_TTL
        })
    }

    /// `key`'s fresh snapshot if `stream` has adopted it already: what a
    /// stream that becomes current again holds (PR #106 review N1).
    pub(crate) fn adopted_fresh(
        &self,
        key: &ConnKey,
        stream: StreamId,
        now: Instant,
    ) -> Option<Snapshot> {
        self.entries
            .iter()
            .find(|staged| staged.key == *key && staged.adopted_by.contains(&stream))
            .filter(|staged| now.saturating_duration_since(staged.at) <= PENDING_SNAPSHOT_TTL)
            .map(|staged| staged.snapshot.clone())
    }

    /// `key`'s snapshot for `stream`: `Some` only when it is fresh and this
    /// stream hasn't adopted it yet. A stale one is removed.
    pub(crate) fn adopt_fresh(
        &mut self,
        key: &ConnKey,
        stream: StreamId,
        now: Instant,
    ) -> Option<Snapshot> {
        let index = self.entries.iter().position(|staged| staged.key == *key)?;
        if now.saturating_duration_since(self.entries[index].at) > PENDING_SNAPSHOT_TTL {
            self.entries.remove(index);
            return None;
        }
        let staged = &mut self.entries[index];
        if staged.adopted_by.contains(&stream) {
            return None;
        }
        staged.adopted_by.push(stream);
        Some(staged.snapshot.clone())
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Broadcast the full list as `SetRules`, empty while the cache is Unknown
/// (so a GUI's stale list, or a switch it flipped on one, is reset, issue
/// #61), then what the list leaves out. Sent with the cache lock held, so
/// two publishers can't deliver lists out of order.
pub fn publish_rules(cache: &StdMutex<RulesCache>, broadcast: &broadcast::Sender<ServerMessage>) {
    let cache = lock(cache);
    let rules = cache.snapshot_wire().unwrap_or_default();
    let _ = broadcast.send(ServerMessage::SetRules { rules });
    let _ = broadcast.send(cache.not_shown());
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

/// The part of a `Subscribe` snapshot that may be staged: only its count
/// when it has too many rules, otherwise the rules within the field limits
/// and what was left out.
fn bounded_snapshot(rules: Vec<Rule>) -> Snapshot {
    if rules.len() > MAX_SNAPSHOT_RULES {
        warn!(
            count = rules.len(),
            "daemon rule snapshot too large; staged as its count only"
        );
        return Snapshot {
            over_limit: Some(rules.len()),
            ..Snapshot::default()
        };
    }
    let mut snapshot = Snapshot::default();
    for (index, rule) in rules.into_iter().enumerate() {
        if within_limits(&rule) {
            snapshot.rules.push(rule);
            continue;
        }
        snapshot.left_out_temporary |= count::is_temporary(&rule.duration);
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
    snapshot
}

/// `UiService`'s rule state: the cache, the staged snapshots, a generation
/// bumped on every commit, the broadcast `SetRules` goes out on, and the
/// per-rule hit counts that follow the list (a committed snapshot, a
/// confirmed `DELETE_RULE` and an expired temporary rule prune them;
/// [`Self::withdraw`] never does).
#[derive(Clone)]
pub struct RulesSync {
    cache: SharedRulesCache,
    pending: Arc<StdMutex<PendingSnapshots>>,
    synced: Arc<watch::Sender<u64>>,
    broadcast: broadcast::Sender<ServerMessage>,
    hits: RuleHitsHandle,
    /// Live [`PublishHold`]s; see [`RulesSync::hold_publishes`].
    holds: Arc<AtomicUsize>,
    /// A confirmed command changed the cache while a hold was alive.
    held_changes: Arc<AtomicBool>,
}

/// While any is alive, confirmed commands update the cache without
/// broadcasting `SetRules`; dropping the last one broadcasts the list once,
/// and only if a confirmed command changed it meanwhile (a pass whose
/// commands all failed must not wake every listener, PR #105 re-review).
pub struct PublishHold(RulesSync);

impl Drop for PublishHold {
    fn drop(&mut self) {
        if self.0.holds.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.publish_held_changes();
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
            held_changes: Arc::default(),
        }
    }

    pub fn cache(&self) -> SharedRulesCache {
        self.cache.clone()
    }

    /// Per-rule hit counts (`cache::rule_hits`).
    pub fn hits(&self) -> RuleHitsHandle {
        self.hits.clone()
    }

    /// Counts the events of a ping that carried statistics, with its
    /// `uptime` and `rule_hits`.
    pub fn record_hits(
        &self,
        events: &[snitchwatch_proto::protocol::Event],
        uptime: u64,
        rule_hits: u64,
    ) {
        self.hits.record(events, uptime, rule_hits, &self.cache);
    }

    /// Generation bumped each time a daemon snapshot is committed.
    pub fn synced(&self) -> watch::Receiver<u64> {
        self.synced.subscribe()
    }

    /// A remembered prompt verdict (see [`RulesCache::upsert`]), announced
    /// to every GUI as `UpdateRules`. When it replaces a temporary rule that
    /// has expired but not yet been pruned (names are deterministic,
    /// `verdict::rule_name_for`), it is a new rule and its count starts
    /// again. Only a list gets it (PR #106 review H1): with none, a GUI would
    /// add it to its empty list and lose it at the next `SetRules`. Sent with
    /// the cache lock held, like every list, so a withdrawal can't overtake
    /// it.
    pub fn upsert(&self, rule: Rule) {
        let mut cache = lock(&self.cache);
        if cache.is_unknown() {
            return;
        }
        let replaces_expired = cache
            .expiry_of(&rule.name)
            .is_some_and(|at| at <= rule.created);
        if replaces_expired {
            self.hits.forget([rule.name.as_str()]);
        }
        let name = rule.name.clone();
        cache.note_prompt_answer(&name);
        cache.upsert(rule);
        let rules = cache
            .rules()
            .and_then(|r| r.get(&name))
            .map(|r| cache.wire(r));
        let _ = self.broadcast.send(ServerMessage::UpdateRules {
            rules: rules.into_iter().collect(),
        });
    }

    /// Look up the account names of the `user.name` uids in `rules` that
    /// aren't known yet, so the list can show them (`accounts`, PR #106
    /// review M4). Before a snapshot is staged, on a blocking thread, and
    /// waited for at most [`ACCOUNT_LOOKUP_WAIT`]: the daemon gives
    /// `Subscribe` 10 s and redials after that (`notifications.go`), and NSS
    /// can be slow. Lookups that finish later are still remembered, and a
    /// list already shown is published again with the names.
    pub async fn learn_account_names(&self, lookup: &AccountLookup, rules: &[Rule]) {
        let wanted = {
            let uids = rules.iter().flat_map(user_name_uids).collect();
            lock(&self.cache).accounts.claim(uids)
        };
        if wanted.is_empty() {
            return;
        }
        let (lookup, cache, broadcast) =
            (lookup.clone(), self.cache.clone(), self.broadcast.clone());
        let learning = tokio::task::spawn_blocking(move || {
            let found = look_up_blocking(&lookup, wanted);
            let named = found.iter().any(|(_, name)| name.is_some());
            let listed = {
                let mut cache = lock(&cache);
                cache.accounts.learn(found);
                !cache.is_unknown()
            };
            if named && listed {
                publish_rules(&cache, &broadcast);
            }
        });
        if tokio::time::timeout(ACCOUNT_LOOKUP_WAIT, learning)
            .await
            .is_err()
        {
            warn!("account names are still being looked up; the list shows them when found");
        }
    }

    /// Hold a `Subscribe`'s rules until its connection sends HELLO. An
    /// oversized snapshot is held as its count, replacing the key's earlier
    /// one like any other.
    pub fn stage(&self, key: ConnKey, rules: Vec<Rule>) {
        let snapshot = bounded_snapshot(rules);
        lock(&self.pending).stage(key, snapshot, Instant::now());
    }

    /// Adopt the connection's fresh staged snapshot, if any, and broadcast
    /// it. `current` proves the HELLO's stream is current and the stream
    /// lock is held. An oversized snapshot leaves no list and shows its
    /// count, until its stream closes or stops being current
    /// ([`Self::withdraw`], PR #106 review L1).
    pub(crate) fn commit(&self, current: &BecameCurrent<'_>) -> Adopted {
        let staged =
            lock(&self.pending).adopt_fresh(&current.conn(), current.stream(), Instant::now());
        match staged {
            Some(snapshot) => self.adopt(current, snapshot),
            None => Adopted::Nothing,
        }
    }

    /// The committing stream closed and `current` is current instead: if it
    /// adopted the same staged snapshot, it holds the list from now on, as
    /// adopted again from that snapshot (PR #106 review N1: either HELLO
    /// order on the Unix socket's one key). [`Adopted::Nothing`] otherwise,
    /// and the caller withdraws.
    pub(crate) fn readopt(&self, current: &BecameCurrent<'_>) -> Adopted {
        let staged =
            lock(&self.pending).adopted_fresh(&current.conn(), current.stream(), Instant::now());
        match staged {
            Some(snapshot) => self.adopt(current, snapshot),
            None => Adopted::Nothing,
        }
    }

    /// Whether the cache holds a list (not only an over-limit count).
    pub(crate) fn has_list(&self) -> bool {
        !lock(&self.cache).is_unknown()
    }

    fn adopt(&self, current: &BecameCurrent<'_>, snapshot: Snapshot) -> Adopted {
        if let Some(total) = snapshot.over_limit {
            warn!(
                stream = current.stream(),
                total, "the daemon's rule snapshot is over the limit"
            );
            let mut cache = lock(&self.cache);
            if !cache.is_unknown() {
                cache.set_unknown();
                let _ = self
                    .broadcast
                    .send(ServerMessage::SetRules { rules: Vec::new() });
            }
            cache.over_limit_total = Some(total);
            let _ = self.broadcast.send(cache.not_shown());
            return Adopted::OverLimit;
        }
        info!(
            stream = current.stream(),
            rules = snapshot.rules.len(),
            "adopted the daemon's rule snapshot"
        );
        {
            let mut cache = lock(&self.cache);
            cache.replace_all(snapshot.rules);
            cache.set_left_out(snapshot.left_out);
            cache.note_left_out_temporary(snapshot.left_out_temporary);
            self.hits.adopt_snapshot(&cache);
        }
        self.publish();
        self.synced.send_modify(|generation| *generation += 1);
        Adopted::List
    }

    /// The stream the list came from is gone or no longer current: forget
    /// the list and clear it in every GUI, so none of its rows can be acted
    /// on under another stream.
    pub(crate) fn withdraw(&self) {
        let mut cache = lock(&self.cache);
        // An over-limit count belongs to its stream too (PR #106 review L1).
        let had_count = cache.over_limit_total.take().is_some();
        if cache.is_unknown() {
            if had_count {
                let _ = self.broadcast.send(cache.not_shown());
            }
            return;
        }
        cache.set_unknown();
        info!("withdrew the daemon rule list; its stream is gone");
        let _ = self
            .broadcast
            .send(ServerMessage::SetRules { rules: Vec::new() });
        let _ = self.broadcast.send(cache.not_shown());
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
        self.publish_after_command();
    }

    /// Publish a daemon-answered command's change: at once, or once the
    /// last [`PublishHold`] drops.
    fn publish_after_command(&self) {
        if self.holds.load(Ordering::SeqCst) == 0 {
            self.publish();
        } else {
            self.held_changes.store(true, Ordering::SeqCst);
            // The last hold may have dropped since the load above.
            if self.holds.load(Ordering::SeqCst) == 0 {
                self.publish_held_changes();
            }
        }
    }

    fn publish_held_changes(&self) {
        if self.held_changes.swap(false, Ordering::SeqCst) {
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

/// Every `period`, prune expired temporary rules, forget their hit counts
/// and broadcast the list if anything went. Ends once the cache itself is
/// gone (bridge shut down).
pub async fn prune_expired_rules_every(
    period: Duration,
    cache: Weak<StdMutex<RulesCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
    hits: RuleHitsHandle,
) {
    let mut ticks = tokio::time::interval(period);
    loop {
        ticks.tick().await;
        let Some(cache) = cache.upgrade() else {
            return;
        };
        let pruned = {
            let mut cache = lock(&cache);
            let expired = cache.prune_expired(now_secs());
            // Under the cache lock, like every hit-count change. A rule
            // re-made under the same name later is a new rule.
            hits.forget(expired.iter().map(String::as_str));
            !expired.is_empty()
        };
        if pruned {
            publish_rules(&cache, &broadcast);
        }
    }
}

#[path = "rules_count.rs"]
mod count;
#[path = "rules_refused.rs"]
mod refused;
pub use refused::MAX_FILES_LEFT;

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rules_revision_tests.rs"]
mod revision_tests;

#[cfg(test)]
#[path = "rules_refused_tests.rs"]
mod refused_tests;
