//! `UiService`'s rule state around [`RulesCache`]: the per-connection
//! staged snapshots ([`PendingSnapshots`]), the commit on HELLO
//! ([`RulesSync`]), and the publishers that broadcast the list.

use super::{bounded_snapshot, lock, now_secs, Adopted, RulesCache, SharedRulesCache, Snapshot};
use crate::accounts::{look_up_blocking, user_name_uids, AccountLookup};
use crate::cache::rule_hits_handle::RuleHitsHandle;
use crate::daemon_commands::{BecameCurrent, CommandError, ConnKey, PendingReply, StreamId};
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
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
    pub(super) entries: VecDeque<Staged>,
}

#[derive(Debug)]
pub(super) struct Staged {
    pub(super) key: ConnKey,
    pub(super) at: Instant,
    pub(super) snapshot: Snapshot,
    pub(super) adopted_by: Vec<StreamId>,
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

/// `UiService`'s rule state: the cache, the staged snapshots, a generation
/// bumped on every commit, the broadcast `SetRules` goes out on, and the
/// per-rule hit counts that follow the list (a committed snapshot, a
/// confirmed `DELETE_RULE` and an expired temporary rule prune them;
/// [`Self::withdraw`] never does).
#[derive(Clone)]
pub struct RulesSync {
    pub(super) cache: SharedRulesCache,
    pub(super) pending: Arc<StdMutex<PendingSnapshots>>,
    synced: Arc<watch::Sender<u64>>,
    pub(super) broadcast: broadcast::Sender<ServerMessage>,
    pub(super) hits: RuleHitsHandle,
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
        self.stage_at_revision(key, rules, self.snapshot_revision());
    }

    /// Capture before any await while handling a received Subscribe.
    pub(crate) fn snapshot_revision(&self) -> u64 {
        lock(&self.cache).revision()
    }

    /// Stage with the immutable receipt cutoff, even if account lookup or
    /// other work let newer refusals arrive before staging.
    pub(crate) fn stage_at_revision(&self, key: ConnKey, rules: Vec<Rule>, observed_revision: u64) {
        let mut snapshot = bounded_snapshot(rules);
        snapshot.observed_revision = observed_revision;
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
            cache.replace_all_at_revision(snapshot.rules, snapshot.observed_revision);
            cache.set_left_out_at_revision(snapshot.left_out, snapshot.observed_revision);
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
    pub(super) fn publish_after_command(&self) {
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
