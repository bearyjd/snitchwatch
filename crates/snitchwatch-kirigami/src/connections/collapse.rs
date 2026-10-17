//! Repeated daemon events collapse into one row (issue #55; owner decision
//! 2026-10-16: the GUI store only, never the bridge history or exports).
//!
//! A stored rule refusing a connection reports every TCP SYN retransmit as
//! its own daemon event, so one request could list 20 "denied" rows.
//! [`RowCollapse`] sits in front of the row store and the group tree and
//! rewrites each connection [`ServerMessage`]: a decided event row that
//! repeats the latest row with the same [`CollapseKey`] within
//! [`COLLAPSE_WINDOW_MS`] of its last occurrence becomes an update of that
//! row (its *representative*) instead of a new row. The representative keeps
//! its id, row and place, so the selection and the inspector stay on it; the
//! `repeatCount` and `lastSeenMs` roles read [`RowCollapse::repeat_count`]
//! and [`RowCollapse::last_seen_ms`].
//!
//! Safety rules (the user must never miss a prompt or see a wrong verdict):
//! - Only daemon event rows (`event-…`, or `<session>:event-…` once the
//!   model qualified the id) that are decided and not deferred take part.
//!   Pending rows and Ask rows are never absorbed and never absorb anything.
//! - The key holds the raw action string, the rule and every flow field, so
//!   rows with different verdicts, rules or destinations never merge.
//! - An update that changes a row's key releases it (and, for a
//!   representative, every row it absorbed) as separate rows again.
//!
//! Remembered-answer twins (#114.3, a heuristic): for an answer other than
//! "once" the daemon also reports the asked connection as an event, under
//! the rule the answer made, so it was listed twice. Exact pairing needs the
//! pid and source port, which `ConnectionRow` does not carry (adding them,
//! or dropping the event in the bridge, is possible future bridge work). So
//! the first event that matches a decided Ask row on the whole
//! [`CollapseKey`] (same action, the answer's own rule name, same program,
//! host, IP, port and protocol) and starts within [`TWIN_WINDOW_MS`] of the
//! Ask folds into that Ask row: it is counted there, not hidden. If the
//! real twin never comes, the app's next connection under that rule is the
//! one folded: still counted, never lost, never across verdicts. At most
//! one event folds into an Ask row.

use std::collections::HashMap;

use snitchwatch_bridge::deferred_answers::ANSWER_TIMEOUT;
use snitchwatch_bridge::translator::connection::EVENT_ROW_PREFIX;
use snitchwatch_bridge::translator::verdict::is_answer_name;
use snitchwatch_bridge::ws_messages::{AutoAnswer, ConnectionRow, ServerMessage};

use super::outcome::is_pending;

/// How close (ms) a repeat must be to the last occurrence of its key to join
/// that row (owner decision on #55: 10 s).
pub const COLLAPSE_WINDOW_MS: i64 = 10_000;

/// How long after an Ask row's start the daemon's event for its answer may
/// come (#114.3): the bridge answers by itself after `ANSWER_TIMEOUT`, plus
/// [`COLLAPSE_WINDOW_MS`] of slack for the daemon's report.
pub const TWIN_WINDOW_MS: i64 = ANSWER_TIMEOUT.as_millis() as i64 + COLLAPSE_WINDOW_MS;

/// What two rows must share to collapse: the app, the destination, the port
/// and the verdict (owner's list), made stricter with the rule, protocol and
/// direction. A stricter key only ever merges less.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollapseKey {
    action: Option<String>,
    decided_by_default: bool,
    matched_rule: Option<String>,
    auto_answer: Option<AutoAnswerKey>,
    process: String,
    process_path: Option<String>,
    dst_host: String,
    dst_ip: String,
    dst_port: u16,
    protocol: String,
    direction: String,
}

/// `AutoAnswer` without deriving `Hash` on the wire type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AutoAnswerKey {
    FilterPaused,
    NoAnswer,
    Unknown,
}

impl CollapseKey {
    fn of(row: &ConnectionRow) -> Self {
        Self {
            action: row.action.clone(),
            decided_by_default: row.decided_by_default,
            matched_rule: row.matched_rule.clone(),
            auto_answer: row.auto_answer.map(|why| match why {
                AutoAnswer::FilterPaused => AutoAnswerKey::FilterPaused,
                AutoAnswer::NoAnswer => AutoAnswerKey::NoAnswer,
                AutoAnswer::Unknown => AutoAnswerKey::Unknown,
            }),
            process: row.process.clone(),
            process_path: row.process_path.clone(),
            dst_host: row.dst_host.clone(),
            dst_ip: row.dst_ip.clone(),
            dst_port: row.dst_port,
            protocol: row.protocol.clone(),
            direction: row.direction.clone(),
        }
    }
}

/// Whether `id` names a daemon event row, bare or session-qualified.
pub fn is_event_row_id(id: &str) -> bool {
    let bare = match id.split_once(':') {
        Some((session, rest)) if session.parse::<u64>().is_ok() => rest,
        _ => id,
    };
    bare.starts_with(EVENT_ROW_PREFIX)
}

/// Whether `row` may take part in a collapse at all.
fn collapsible(row: &ConnectionRow) -> bool {
    is_event_row_id(&row.id) && row.action.is_some() && !row.deferred && !is_pending(row)
}

/// A shown row that absorbed (or may absorb) repeats.
#[derive(Debug)]
struct Representative {
    row: ConnectionRow,
    key: CollapseKey,
    first_seen_ms: i64,
    last_seen_ms: i64,
    /// The absorbed rows, in arrival order. Only id and start are kept:
    /// every other field shown matches `row` (same key), so a released or
    /// promoted row is rebuilt from it ([`Representative::member_row`]).
    members: Vec<Member>,
}

#[derive(Debug, Clone)]
struct Member {
    id: String,
    started_at_ms: i64,
}

impl Member {
    fn of(row: &ConnectionRow) -> Self {
        Self {
            id: row.id.clone(),
            started_at_ms: row.started_at_ms,
        }
    }
}

impl Representative {
    fn new(row: ConnectionRow) -> Self {
        Self {
            key: CollapseKey::of(&row),
            first_seen_ms: row.started_at_ms,
            last_seen_ms: row.started_at_ms,
            row,
            members: Vec::new(),
        }
    }

    fn member_row(&self, member: &Member) -> ConnectionRow {
        ConnectionRow {
            id: member.id.clone(),
            started_at_ms: member.started_at_ms,
            ..self.row.clone()
        }
    }

    fn takes(&self, row: &ConnectionRow) -> bool {
        row.started_at_ms >= self.first_seen_ms - COLLAPSE_WINDOW_MS
            && row.started_at_ms <= self.last_seen_ms + COLLAPSE_WINDOW_MS
    }
}

/// The collapse state; see the module docs.
#[derive(Debug, Default)]
pub struct RowCollapse {
    /// Representatives by id: every one with members, plus the open one per
    /// key.
    reps: HashMap<String, Representative>,
    /// The latest representative of each key still taking repeats.
    open: HashMap<CollapseKey, String>,
    /// Absorbed row id -> its representative's id.
    member_of: HashMap<String, String>,
    /// Decided Ask rows that may still take their twin, by id.
    asks: HashMap<String, ConnectionRow>,
    /// The `at` [`RowCollapse::expire`] last ran for.
    expired_at: Option<i64>,
}

/// Messages produced while rewriting one incoming message.
#[derive(Default)]
struct Out {
    inserted: Vec<ConnectionRow>,
    updated: Vec<ConnectionRow>,
    removed: Vec<String>,
    /// Rows to (re)insert after removals: promoted or released rows.
    reinserted: Vec<ConnectionRow>,
}

impl Out {
    fn touch(&mut self, row: &ConnectionRow) {
        match self.updated.iter_mut().find(|r| r.id == row.id) {
            Some(slot) => *slot = row.clone(),
            None => self.updated.push(row.clone()),
        }
    }

    fn into_messages(self) -> Vec<ServerMessage> {
        let mut msgs = Vec::new();
        if !self.removed.is_empty() {
            msgs.push(ServerMessage::RemoveConnectionRows { ids: self.removed });
        }
        if !self.inserted.is_empty() {
            msgs.push(ServerMessage::InsertConnectionRows {
                rows: self.inserted,
            });
        }
        if !self.reinserted.is_empty() {
            msgs.push(ServerMessage::InsertConnectionRows {
                rows: self.reinserted,
            });
        }
        if !self.updated.is_empty() {
            msgs.push(ServerMessage::UpdateConnectionRows { rows: self.updated });
        }
        msgs
    }
}

impl RowCollapse {
    /// How many rows `id` stands for (1 when it absorbed none).
    pub fn repeat_count(&self, id: &str) -> usize {
        self.reps.get(id).map_or(1, |rep| 1 + rep.members.len())
    }

    /// When `id`'s key was last seen, if `id` is a representative.
    pub fn last_seen_ms(&self, id: &str) -> Option<i64> {
        self.reps.get(id).map(|rep| rep.last_seen_ms)
    }

    /// Entries held: representatives, open keys, absorbed ids, waiting asks.
    #[cfg(test)]
    fn state_size(&self) -> usize {
        self.reps.len() + self.open.len() + self.member_of.len() + self.asks.len()
    }

    /// Stop every open row from taking repeats ("From now" was turned on:
    /// a row after the cut must not hide inside one from before it).
    pub fn seal(&mut self) {
        self.open.clear();
        self.asks.clear();
        self.reps.retain(|_, rep| !rep.members.is_empty());
    }

    /// Rewrite one connection message for the row store and group tree.
    pub fn rewrite(&mut self, msg: ServerMessage) -> Vec<ServerMessage> {
        let mut out = Out::default();
        match msg {
            ServerMessage::InsertConnectionRows { rows } => {
                for row in rows {
                    self.insert(row, &mut out);
                }
            }
            ServerMessage::UpdateConnectionRows { rows } => {
                for row in rows {
                    if !self.update_known(&row, &mut out) {
                        out.touch(&row);
                    }
                }
            }
            ServerMessage::RemoveConnectionRows { ids } => {
                for id in ids {
                    self.remove(id, &mut out);
                }
            }
            ServerMessage::MoveConnetionRows { ids } => {
                let mut moved: Vec<String> = Vec::with_capacity(ids.len());
                for id in ids {
                    let id = self.member_of.get(&id).cloned().unwrap_or(id);
                    if !moved.contains(&id) {
                        moved.push(id);
                    }
                }
                return vec![ServerMessage::MoveConnetionRows { ids: moved }];
            }
            ServerMessage::ClearConnectionRows => {
                *self = Self::default();
                return vec![ServerMessage::ClearConnectionRows];
            }
            other => return vec![other],
        }
        out.into_messages()
    }

    fn insert(&mut self, row: ConnectionRow, out: &mut Out) {
        if self.update_known(&row, out) {
            return;
        }
        if !collapsible(&row) {
            self.note_ask(&row);
            out.inserted.push(row);
            return;
        }
        let key = CollapseKey::of(&row);
        self.expire(row.started_at_ms);
        if let Some(ask_id) = self.twin_ask(&key, row.started_at_ms) {
            self.fold_twin(ask_id, row, out);
            return;
        }
        let open = self.open.get(&key).cloned();
        if let Some(rep_id) = open {
            if let Some(rep) = self.reps.get_mut(&rep_id) {
                if rep.takes(&row) {
                    rep.last_seen_ms = rep.last_seen_ms.max(row.started_at_ms);
                    self.member_of.insert(row.id.clone(), rep_id);
                    rep.members.push(Member::of(&row));
                    out.touch(&rep.row);
                    return;
                }
            }
            self.forget_if_alone(&rep_id);
        }
        self.open.insert(key, row.id.clone());
        self.reps
            .insert(row.id.clone(), Representative::new(row.clone()));
        out.inserted.push(row);
    }

    /// Apply `row` to a representative or absorbed row of the same id.
    /// False when the id is neither.
    fn update_known(&mut self, row: &ConnectionRow, out: &mut Out) -> bool {
        if let Some(rep_id) = self.member_of.get(&row.id).cloned() {
            let Some(rep) = self.reps.get_mut(&rep_id) else {
                self.member_of.remove(&row.id);
                return false;
            };
            let Some(at) = rep.members.iter().position(|m| m.id == row.id) else {
                self.member_of.remove(&row.id);
                return false;
            };
            if collapsible(row) && CollapseKey::of(row) == rep.key {
                rep.members[at] = Member::of(row);
            } else {
                rep.members.remove(at);
                self.member_of.remove(&row.id);
                out.touch(&rep.row);
                out.reinserted.push(row.clone());
            }
            return true;
        }
        let Some(rep) = self.reps.get_mut(&row.id) else {
            self.note_ask(row);
            return false;
        };
        let takes_part = collapsible(row) || !is_event_row_id(&row.id);
        if takes_part && CollapseKey::of(row) == rep.key {
            rep.row = row.clone();
        } else if let Some(rep) = self.reps.remove(&row.id) {
            self.close(&rep.key, &row.id);
            for member in &rep.members {
                self.member_of.remove(&member.id);
                out.reinserted.push(rep.member_row(member));
            }
        }
        out.touch(row);
        true
    }

    fn remove(&mut self, id: String, out: &mut Out) {
        self.asks.remove(&id);
        if let Some(rep_id) = self.member_of.remove(&id) {
            if let Some(rep) = self.reps.get_mut(&rep_id) {
                rep.members.retain(|m| m.id != id);
                out.touch(&rep.row);
            }
            return;
        }
        if let Some(rep) = self.reps.remove(&id) {
            let was_open = self.open.get(&rep.key) == Some(&id);
            self.close(&rep.key, &id);
            let mut members = rep.members.iter();
            if let Some(first) = members.next().map(|m| rep.member_row(m)) {
                // The oldest repeat takes the removed row's place.
                let promoted_id = first.id.clone();
                self.member_of.remove(&promoted_id);
                let mut promoted = Representative::new(first.clone());
                promoted.last_seen_ms = rep.last_seen_ms;
                for member in members {
                    self.member_of
                        .insert(member.id.clone(), promoted_id.clone());
                    promoted.members.push(member.clone());
                }
                if was_open {
                    self.open.insert(rep.key.clone(), promoted_id.clone());
                }
                self.reps.insert(promoted_id, promoted);
                out.reinserted.push(first);
            }
        }
        // A row this same message removes must not come back as an insert or
        // an update (PR #167 review: Remove[rep, repeat] in either order).
        out.reinserted.retain(|row| row.id != id);
        out.updated.retain(|row| row.id != id);
        out.removed.push(id);
    }

    /// Track `row` as an Ask row that may take a twin, or stop tracking it.
    fn note_ask(&mut self, row: &ConnectionRow) {
        let waits = !is_event_row_id(&row.id)
            && !row.deferred
            && row.action.is_some()
            && !is_pending(row)
            && row.matched_rule.as_deref().is_some_and(is_answer_name);
        if waits {
            self.asks.insert(row.id.clone(), row.clone());
        } else {
            self.asks.remove(&row.id);
        }
    }

    /// The decided Ask row an event of `key` starting at `at` is the twin of.
    fn twin_ask(&self, key: &CollapseKey, at: i64) -> Option<String> {
        self.asks
            .values()
            .filter(|ask| {
                at >= ask.started_at_ms
                    && at <= ask.started_at_ms + TWIN_WINDOW_MS
                    && CollapseKey::of(ask) == *key
            })
            .min_by(|a, b| (a.started_at_ms, &a.id).cmp(&(b.started_at_ms, &b.id)))
            .map(|ask| ask.id.clone())
    }

    /// Fold `twin` into Ask row `ask_id` (its only twin).
    fn fold_twin(&mut self, ask_id: String, twin: ConnectionRow, out: &mut Out) {
        let Some(ask) = self.asks.remove(&ask_id) else {
            return;
        };
        let rep = self
            .reps
            .entry(ask_id.clone())
            .or_insert_with(|| Representative::new(ask));
        rep.last_seen_ms = rep.last_seen_ms.max(twin.started_at_ms);
        self.member_of.insert(twin.id.clone(), ask_id);
        rep.members.push(Member::of(&twin));
        out.touch(&rep.row);
    }

    /// Forget what can no longer take a row starting at `at` (PR #167
    /// review: the bridge evicts history without telling GUIs, so time is
    /// the only bound). Runs at most once per [`COLLAPSE_WINDOW_MS`] of `at`:
    /// open rows whose window ended (alone ones entirely) and Asks past
    /// their twin window.
    fn expire(&mut self, at: i64) {
        // Throttled only within one window after the last run: an `at`
        // before it (a far-future row came first, clock skew) runs too, and
        // keeps everything still in its window, so it is safe.
        if self
            .expired_at
            .is_some_and(|last| (last..last + COLLAPSE_WINDOW_MS).contains(&at))
        {
            return;
        }
        self.expired_at = Some(at);
        let reps = &mut self.reps;
        self.open.retain(|_, id| {
            let Some(rep) = reps.get(id) else {
                return false;
            };
            if rep.last_seen_ms + COLLAPSE_WINDOW_MS >= at {
                return true;
            }
            if rep.members.is_empty() {
                reps.remove(id);
            }
            false
        });
        self.asks
            .retain(|_, ask| ask.started_at_ms + 2 * TWIN_WINDOW_MS >= at);
    }

    /// Stop `id` taking repeats of `key`.
    fn close(&mut self, key: &CollapseKey, id: &str) {
        if self.open.get(key).is_some_and(|open| open == id) {
            self.open.remove(key);
        }
    }

    /// A representative that absorbed nothing is only kept while open.
    fn forget_if_alone(&mut self, id: &str) {
        if self.reps.get(id).is_some_and(|rep| rep.members.is_empty()) {
            self.reps.remove(id);
        }
    }
}

#[cfg(test)]
mod tests;
