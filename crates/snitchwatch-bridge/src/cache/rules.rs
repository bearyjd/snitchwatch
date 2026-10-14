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

use crate::accounts::KnownAccounts;
use crate::rule_wire::rule_to_wire;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};
use tracing::warn;

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
    /// Cache revision of each latest refused delete. A retained Subscribe
    /// predating that event cannot prove the file was loaded again.
    files_left_revisions: BTreeMap<String, u64>,
    /// Marked names the daemon took back as an `always` rule since the mark
    /// (`rules_refused.rs`): a delete `OK` of one removed its file.
    files_left_held: BTreeSet<String>,
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
    ///
    /// **An assumption this rests on:** Rust's `Instant` on Linux reads
    /// `CLOCK_MONOTONIC`, not `CLOCK_BOOTTIME`, and Go's runtime timers
    /// (`nanotime`) read the same clock, so the two stop together while the
    /// host is suspended. If either ever counted suspend time, the tolerance
    /// for a rule pruned early would silently stop working.
    ends: Instant,
}

/// A `Subscribe` snapshot within the limits, and what it left out; or, for
/// one over [`MAX_SNAPSHOT_RULES`], only how many rules it had.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    /// Cache revision when Subscribe was received, before later refusals.
    observed_revision: u64,
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
        self.set_left_out_at_revision(left_out, self.revision);
    }

    fn set_left_out_at_revision(
        &mut self,
        left_out: BTreeMap<String, usize>,
        observed_revision: u64,
    ) {
        for name in left_out.keys() {
            self.forget_file_left_in_snapshot(name, observed_revision);
        }
        self.left_out = left_out;
    }

    /// Adopt a daemon snapshot. An enabled temporary rule's timer is taken
    /// to have started at its `created` (the daemon's stamp of its last
    /// change, when the timer started); a disabled one is given none, since
    /// whether one still runs isn't known.
    pub fn replace_all(&mut self, rules: Vec<Rule>) {
        self.replace_all_at_revision(rules, self.revision);
    }

    fn replace_all_at_revision(&mut self, rules: Vec<Rule>, observed_revision: u64) {
        let clock = Instant::now();
        self.expiries = rules
            .iter()
            .filter(|rule| rule.enabled && rule.created > 0)
            .filter_map(|rule| Some((rule.name.clone(), timer_from(rule, rule.created, clock)?)))
            .collect();
        for rule in &rules {
            // Only a list observed after the refusal proves the file loaded.
            self.forget_file_left_in_snapshot(&rule.name, observed_revision);
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
                .then(|| timer_from(&rule, now_secs, Instant::now()))
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
                } else {
                    // Held as a temporary rule now: `Delete` removes no file.
                    self.files_left_held.remove(&rule.name);
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
                self.forget_file_left_if_held(&rule.name);
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
///
/// `ends` is `clock` plus the **whole** duration, whatever `start` is. For a
/// change the bridge made, `start` is now, so that is when the daemon's timer
/// fires. For a rule in a snapshot, `start` is its `created` stamp, which
/// the daemon's timer need not have started at: a rule loaded from a
/// temporary-rule file starts its timer at load (`loadRule`) with the file's
/// old stamp, and a list adopted after the host slept has run its timers on
/// the monotonic clock. The whole duration is an upper bound that hides at
/// most one duration.
fn timer_from(rule: &Rule, start: i64, clock: Instant) -> Option<Expiry> {
    if !count::is_temporary(&rule.duration) {
        return None;
    }
    let secs = parse_duration_secs(&rule.duration)?;
    let at = start.checked_add(secs)?;
    let whole = Duration::from_secs(u64::try_from(secs).unwrap_or(0));
    Some(Expiry {
        duration: rule.duration.clone(),
        at,
        ends: clock.checked_add(whole).unwrap_or(clock),
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

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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

#[path = "rules_sync.rs"]
mod sync;
pub use sync::{
    prune_expired_rules_every, publish_rules, settle_rule_command, PendingSnapshots, PublishHold,
    RulesSync,
};

#[path = "rules_count.rs"]
mod count;
#[path = "rules_refused.rs"]
mod refused;
pub use refused::{applied_but_not_saved, MAX_FILES_LEFT};

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "rules_revision_tests.rs"]
mod revision_tests;

#[cfg(test)]
#[path = "rules_refused_tests.rs"]
mod refused_tests;
