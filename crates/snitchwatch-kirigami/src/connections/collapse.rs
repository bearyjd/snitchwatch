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
        if self
            .expired_at
            .is_some_and(|last| at < last + COLLAPSE_WINDOW_MS)
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
mod tests {
    use super::*;

    fn event(id: &str, at: i64, action: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "python3".into(),
            process_path: Some("/usr/bin/python3".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: Some(action.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: at,
            matched_rule: Some("deny-python".into()),
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    fn insert(rows: Vec<ConnectionRow>) -> ServerMessage {
        ServerMessage::InsertConnectionRows { rows }
    }

    /// Feed `msgs` and return the rows a plain store would hold, in order.
    fn shown(c: &mut RowCollapse, msgs: Vec<ServerMessage>) -> Vec<String> {
        let mut store = super::super::row_store::RowStore::new();
        for msg in msgs {
            for out in c.rewrite(msg) {
                store.apply(out);
            }
        }
        store.rows().iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn event_row_ids_are_recognised_bare_and_qualified() {
        assert!(is_event_row_id("event-1-2"));
        assert!(is_event_row_id("7:event-1-2"));
        assert!(!is_event_row_id("ask-3"));
        assert!(!is_event_row_id("7:ask-3"));
        assert!(!is_event_row_id("x:event-1"));
    }

    #[test]
    fn repeats_within_the_window_become_one_row_with_a_count() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("event-1", 1_000, "deny")]),
                insert(vec![event("event-2", 2_000, "deny")]),
                insert(vec![event("event-3", 3_500, "deny")]),
            ],
        );
        assert_eq!(ids, ["event-1"]);
        assert_eq!(c.repeat_count("event-1"), 3);
        assert_eq!(c.last_seen_ms("event-1"), Some(3_500));
    }

    #[test]
    fn absorbing_a_repeat_updates_the_representative_in_place() {
        let mut c = RowCollapse::default();
        c.rewrite(insert(vec![event("event-1", 1_000, "deny")]));
        let out = c.rewrite(insert(vec![event("event-2", 2_000, "deny")]));
        assert_eq!(
            out,
            vec![ServerMessage::UpdateConnectionRows {
                rows: vec![event("event-1", 1_000, "deny")]
            }]
        );
    }

    #[test]
    fn the_window_slides_from_the_last_occurrence_and_ends_at_ten_seconds() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("event-1", 0, "deny")]),
                insert(vec![event("event-2", COLLAPSE_WINDOW_MS, "deny")]),
                insert(vec![event("event-3", 2 * COLLAPSE_WINDOW_MS, "deny")]),
                insert(vec![event("event-4", 3 * COLLAPSE_WINDOW_MS + 1, "deny")]),
            ],
        );
        assert_eq!(ids, ["event-1", "event-4"]);
        assert_eq!(c.repeat_count("event-1"), 3);
        assert_eq!(c.repeat_count("event-4"), 1);
    }

    #[test]
    fn an_older_repeat_out_of_order_joins_within_the_window_only() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("event-1", 50_000, "deny")]),
                insert(vec![event("event-2", 50_000 - COLLAPSE_WINDOW_MS, "deny")]),
                insert(vec![event(
                    "event-3",
                    50_000 - COLLAPSE_WINDOW_MS - 1,
                    "deny",
                )]),
            ],
        );
        assert_eq!(ids, ["event-1", "event-3"]);
        assert_eq!(c.last_seen_ms("event-1"), Some(50_000));
    }

    #[test]
    fn different_verdicts_never_merge() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "allow"),
                event("event-3", 1_200, "blocklist"),
                event("event-4", 1_300, "reject"),
                event("event-5", 1_400, "drop"),
            ])],
        );
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn every_key_field_keeps_rows_apart() {
        let base = event("event-0", 1_000, "deny");
        let variants: [fn(&mut ConnectionRow); 11] = [
            |r| r.matched_rule = Some("other-rule".into()),
            |r| r.matched_rule = None,
            |r| r.decided_by_default = true,
            |r| r.auto_answer = Some(AutoAnswer::FilterPaused),
            |r| r.process = "curl".into(),
            |r| r.process_path = Some("/usr/bin/curl".into()),
            |r| r.dst_host = "other.example".into(),
            |r| r.dst_ip = "203.0.113.9".into(),
            |r| r.dst_port = 80,
            |r| r.protocol = "udp".into(),
            |r| r.direction = "incoming".into(),
        ];
        for (i, change) in variants.iter().enumerate() {
            let mut c = RowCollapse::default();
            let mut other = event("event-1", 1_100, "deny");
            change(&mut other);
            let ids = shown(
                &mut c,
                vec![insert(vec![base.clone()]), insert(vec![other])],
            );
            assert_eq!(ids.len(), 2, "variant {i} merged");
        }
    }

    #[test]
    fn pending_deferred_and_ask_rows_never_take_part() {
        let mut c = RowCollapse::default();
        let mut pending = event("event-p", 1_100, "deny");
        pending.action = None;
        let mut deferred = event("event-d", 1_200, "deny");
        deferred.deferred = true;
        let ask = event("ask-7", 1_300, "deny");
        let ask_q = event("3:ask-8", 1_400, "deny");
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("event-1", 1_000, "deny")]),
                insert(vec![pending, deferred, ask, ask_q]),
                insert(vec![event("event-2", 1_500, "deny")]),
            ],
        );
        assert_eq!(
            ids,
            ["event-1", "event-p", "event-d", "ask-7", "3:ask-8"],
            "only event-2 joins event-1"
        );
        assert_eq!(c.repeat_count("event-1"), 2);
        assert_eq!(c.repeat_count("ask-7"), 1);
    }

    const ANSWER_RULE: &str = "snitchwatch-allow-example.com-443-ppython3";

    fn ask(id: &str, at: i64, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            action: action.map(str::to_string),
            matched_rule: action.map(|_| ANSWER_RULE.to_string()),
            ..event(id, at, "allow")
        }
    }

    fn answer_event(id: &str, at: i64) -> ConnectionRow {
        ConnectionRow {
            matched_rule: Some(ANSWER_RULE.into()),
            ..event(id, at, "allow")
        }
    }

    fn decide(id: &str, at: i64) -> ServerMessage {
        ServerMessage::UpdateConnectionRows {
            rows: vec![ask(id, at, Some("allow"))],
        }
    }

    /// #114.3: the event the daemon reports for a remembered answer folds
    /// into the answered Ask row and is counted there.
    #[test]
    fn a_remembered_answer_twin_folds_into_its_ask_row() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![ask("ask-1", 1_000, None)]),
                decide("ask-1", 1_000),
                insert(vec![answer_event("event-2", 1_000 + TWIN_WINDOW_MS)]),
            ],
        );
        assert_eq!(ids, ["ask-1"]);
        assert_eq!(c.repeat_count("ask-1"), 2);
        assert_eq!(c.last_seen_ms("ask-1"), Some(1_000 + TWIN_WINDOW_MS));
    }

    /// #114.3, no twin ever arrives: the app's next genuine connection under
    /// the rule is the one folded; it is counted, never lost; at most one
    /// event folds into an Ask row.
    #[test]
    fn without_a_twin_the_next_connection_is_folded_and_counted_once() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![ask("ask-1", 1_000, Some("allow"))]),
                insert(vec![answer_event("event-2", 5_000)]),
                insert(vec![answer_event("event-3", 6_000)]),
            ],
        );
        assert_eq!(ids, ["ask-1", "event-3"]);
        assert_eq!(c.repeat_count("ask-1"), 2);
        assert_eq!(c.repeat_count("event-3"), 1);
    }

    #[test]
    fn a_twin_never_folds_across_verdicts_rules_or_flows() {
        let deny = ConnectionRow {
            action: Some("deny".into()),
            matched_rule: Some("snitchwatch-deny-example.com-443-ppython3".into()),
            ..answer_event("event-2", 2_000)
        };
        let other_rule = ConnectionRow {
            matched_rule: Some("snitchwatch-allow-other-443".into()),
            ..answer_event("event-3", 2_000)
        };
        let other_port = ConnectionRow {
            dst_port: 80,
            ..answer_event("event-4", 2_000)
        };
        let not_answer = ConnectionRow {
            matched_rule: Some("allow-python".into()),
            ..answer_event("event-5", 2_000)
        };
        for twin in [deny, other_rule, other_port, not_answer] {
            let mut c = RowCollapse::default();
            let mut ask_row = ask("ask-1", 1_000, Some("allow"));
            if twin.matched_rule.as_deref() == Some("allow-python") {
                ask_row.matched_rule = twin.matched_rule.clone();
            }
            let ids = shown(
                &mut c,
                vec![insert(vec![ask_row]), insert(vec![twin.clone()])],
            );
            assert_eq!(ids.len(), 2, "{} folded", twin.id);
            assert_eq!(c.repeat_count("ask-1"), 1);
        }
    }

    #[test]
    fn a_twin_needs_a_decided_ask_and_the_answer_window() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![ask("ask-1", 1_000, None)]),
                insert(vec![answer_event("event-2", 1_500)]),
            ],
        );
        assert_eq!(
            ids,
            ["ask-1", "event-2"],
            "a pending Ask never takes a twin"
        );

        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![ask("ask-1", 1_000, Some("allow"))]),
                insert(vec![answer_event("event-2", 1_000 + TWIN_WINDOW_MS + 1)]),
                insert(vec![answer_event("event-3", 999)]),
            ],
        );
        assert_eq!(ids, ["ask-1", "event-2", "event-3"], "outside the window");
    }

    #[test]
    fn a_folded_twin_is_released_when_the_ask_row_changes() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![ask("ask-1", 1_000, Some("allow"))]),
                insert(vec![answer_event("event-2", 2_000)]),
                ServerMessage::UpdateConnectionRows {
                    rows: vec![ConnectionRow {
                        action: Some("deny".into()),
                        ..ask("ask-1", 1_000, Some("allow"))
                    }],
                },
            ],
        );
        assert_eq!(ids, ["ask-1", "event-2"]);
        assert_eq!(c.repeat_count("ask-1"), 1);
    }

    /// PR #167 review: after "From now" a new connection must not fold into
    /// an Ask row from before the cut (the filter hides that row).
    #[test]
    fn sealing_stops_an_ask_row_taking_a_twin() {
        let mut c = RowCollapse::default();
        let mut store = super::super::row_store::RowStore::new();
        for out in c.rewrite(insert(vec![ask("ask-1", 1_000, Some("allow"))])) {
            store.apply(out);
        }
        c.seal();
        for out in c.rewrite(insert(vec![answer_event("event-2", 2_000)])) {
            store.apply(out);
        }
        let ids: Vec<&str> = store.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["ask-1", "event-2"]);
        assert_eq!(c.repeat_count("ask-1"), 1);
    }

    #[test]
    fn a_deferred_ask_row_never_takes_a_twin() {
        let mut c = RowCollapse::default();
        let deferred = ConnectionRow {
            deferred: true,
            ..ask("ask-1", 1_000, Some("allow"))
        };
        let ids = shown(
            &mut c,
            vec![
                insert(vec![deferred]),
                insert(vec![answer_event("event-2", 2_000)]),
            ],
        );
        assert_eq!(ids, ["ask-1", "event-2"]);
    }

    /// PR #167 review: one Remove naming a representative and its repeat
    /// removes both, in either order; nothing comes back.
    #[test]
    fn removing_a_representative_and_its_repeat_together_leaves_nothing() {
        for ids in [["event-1", "event-2"], ["event-2", "event-1"]] {
            let mut c = RowCollapse::default();
            let mut store = super::super::row_store::RowStore::new();
            let mut tree = super::super::grouping::GroupTree::default();
            for msg in [
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::RemoveConnectionRows {
                    ids: ids.iter().map(|s| s.to_string()).collect(),
                },
            ] {
                for out in c.rewrite(msg) {
                    match &out {
                        ServerMessage::InsertConnectionRows { rows }
                        | ServerMessage::UpdateConnectionRows { rows } => {
                            rows.iter().for_each(|r| tree.upsert_row(r))
                        }
                        ServerMessage::RemoveConnectionRows { ids } => {
                            ids.iter().for_each(|id| tree.remove_row(id))
                        }
                        _ => {}
                    }
                    store.apply(out);
                }
            }
            assert!(store.is_empty(), "{ids:?}: {:?}", store.rows());
            assert_eq!(tree.process_count(), 0, "{ids:?}: the tree kept a row");
            assert_eq!(c.state_size(), 0, "{ids:?}");
        }
    }

    /// PR #167 review: a flood of distinct flows over time keeps the
    /// collapse state bounded by the flows of the last few windows.
    #[test]
    fn a_flood_of_distinct_flows_keeps_the_state_bounded() {
        let mut c = RowCollapse::default();
        let per_second = 50;
        let seconds = 600;
        let asks_per_second = 5;
        for s in 0..seconds {
            let rows = (0..per_second)
                .map(|i| ConnectionRow {
                    dst_port: (s * per_second + i) as u16,
                    ..event(&format!("event-{s}-{i}"), s * 1_000, "deny")
                })
                .collect();
            c.rewrite(insert(rows));
            // Answered Asks that never get a twin wait only so long.
            let asks = (0..asks_per_second)
                .map(|i| ConnectionRow {
                    dst_port: (s * asks_per_second + i) as u16,
                    ..ask(&format!("ask-{s}-{i}"), s * 1_000, Some("allow"))
                })
                .collect();
            c.rewrite(insert(asks));
        }
        // Open row + representative per flow of the last two windows, and
        // the Asks of twice the twin window plus one expiry period.
        let bound = (per_second * 2 * (2 * COLLAPSE_WINDOW_MS / 1_000 + 1)
            + asks_per_second * ((2 * TWIN_WINDOW_MS + COLLAPSE_WINDOW_MS) / 1_000 + 1))
            as usize;
        assert!(
            c.state_size() <= bound,
            "state {} > bound {bound}",
            c.state_size()
        );
    }

    #[test]
    fn an_ask_row_never_absorbs_a_matching_event() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("ask-1", 1_000, "deny")]),
                insert(vec![event("event-2", 1_100, "deny")]),
            ],
        );
        assert_eq!(ids, ["ask-1", "event-2"]);
    }

    #[test]
    fn qualified_event_rows_collapse_too() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![insert(vec![
                event("4:event-1", 1_000, "deny"),
                event("4:event-2", 1_100, "deny"),
            ])],
        );
        assert_eq!(ids, ["4:event-1"]);
        assert_eq!(c.repeat_count("4:event-1"), 2);
    }

    #[test]
    fn one_batch_holding_a_row_and_its_repeats_inserts_one_row() {
        let mut c = RowCollapse::default();
        let out = c.rewrite(insert(vec![
            event("event-1", 1_000, "deny"),
            event("event-2", 1_100, "deny"),
            event("ask-3", 1_200, "deny"),
            event("event-4", 1_300, "deny"),
        ]));
        assert_eq!(
            out,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("ask-3", 1_200, "deny")
                ]),
                ServerMessage::UpdateConnectionRows {
                    rows: vec![event("event-1", 1_000, "deny")]
                },
            ]
        );
        assert_eq!(c.repeat_count("event-1"), 3);
    }

    #[test]
    fn removing_an_absorbed_row_lowers_the_count_only() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::RemoveConnectionRows {
                    ids: vec!["event-2".into()],
                },
            ],
        );
        assert_eq!(ids, ["event-1"]);
        assert_eq!(c.repeat_count("event-1"), 1);
    }

    #[test]
    fn removing_a_representative_promotes_its_oldest_repeat() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                    event("event-3", 1_200, "deny"),
                ]),
                ServerMessage::RemoveConnectionRows {
                    ids: vec!["event-1".into()],
                },
                insert(vec![event("event-4", 1_300, "deny")]),
            ],
        );
        assert_eq!(ids, ["event-2"]);
        assert_eq!(c.repeat_count("event-2"), 3);
        assert_eq!(c.last_seen_ms("event-2"), Some(1_300));
        assert_eq!(c.repeat_count("event-1"), 1);
    }

    #[test]
    fn a_move_of_an_absorbed_row_moves_its_representative() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![event("ask-0", 500, "allow")]),
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::MoveConnetionRows {
                    ids: vec!["event-2".into()],
                },
            ],
        );
        assert_eq!(ids, ["event-1", "ask-0"]);
    }

    #[test]
    fn an_update_that_changes_an_absorbed_row_releases_it() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::UpdateConnectionRows {
                    rows: vec![event("event-2", 1_100, "allow")],
                },
            ],
        );
        assert_eq!(ids, ["event-1", "event-2"]);
        assert_eq!(c.repeat_count("event-1"), 1);
    }

    #[test]
    fn an_update_that_keeps_the_key_stays_absorbed() {
        let mut c = RowCollapse::default();
        let mut later = event("event-2", 1_100, "deny");
        later.bytes_sent = 99;
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::UpdateConnectionRows { rows: vec![later] },
            ],
        );
        assert_eq!(ids, ["event-1"]);
        assert_eq!(c.repeat_count("event-1"), 2);
    }

    #[test]
    fn an_update_that_changes_a_representative_releases_its_repeats() {
        let mut c = RowCollapse::default();
        let mut store = super::super::row_store::RowStore::new();
        for msg in [
            insert(vec![
                event("event-1", 1_000, "deny"),
                event("event-2", 1_100, "deny"),
            ]),
            ServerMessage::UpdateConnectionRows {
                rows: vec![event("event-1", 1_000, "allow")],
            },
            insert(vec![event("event-3", 1_200, "deny")]),
        ] {
            for out in c.rewrite(msg) {
                store.apply(out);
            }
        }
        let shown: Vec<(&str, Option<&str>)> = store
            .rows()
            .iter()
            .map(|r| (r.id.as_str(), r.action.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("event-1", Some("allow")),
                ("event-2", Some("deny")),
                ("event-3", Some("deny"))
            ]
        );
        assert_eq!(c.repeat_count("event-1"), 1);
    }

    #[test]
    fn a_reinsert_of_a_known_id_is_an_update() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                insert(vec![event("event-2", 1_100, "deny")]),
                insert(vec![event("event-1", 1_000, "deny")]),
            ],
        );
        assert_eq!(ids, ["event-1"]);
        assert_eq!(c.repeat_count("event-1"), 2);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut c = RowCollapse::default();
        let ids = shown(
            &mut c,
            vec![
                insert(vec![
                    event("event-1", 1_000, "deny"),
                    event("event-2", 1_100, "deny"),
                ]),
                ServerMessage::ClearConnectionRows,
                insert(vec![event("event-3", 1_200, "deny")]),
            ],
        );
        assert_eq!(ids, ["event-3"]);
        assert_eq!(c.repeat_count("event-1"), 1);
        assert_eq!(c.repeat_count("event-3"), 1);
    }

    #[test]
    fn sealing_starts_a_new_row_for_the_next_repeat() {
        let mut c = RowCollapse::default();
        let mut store = super::super::row_store::RowStore::new();
        for out in c.rewrite(insert(vec![
            event("event-1", 1_000, "deny"),
            event("event-2", 1_100, "deny"),
        ])) {
            store.apply(out);
        }
        c.seal();
        for out in c.rewrite(insert(vec![event("event-3", 1_200, "deny")])) {
            store.apply(out);
        }
        let ids: Vec<&str> = store.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["event-1", "event-3"]);
        assert_eq!(c.repeat_count("event-1"), 2, "sealing keeps the count");
    }
}
