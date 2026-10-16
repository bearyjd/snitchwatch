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

use std::collections::HashMap;

use snitchwatch_bridge::translator::connection::EVENT_ROW_PREFIX;
use snitchwatch_bridge::ws_messages::{AutoAnswer, ConnectionRow, ServerMessage};

use super::outcome::is_pending;

/// How close (ms) a repeat must be to the last occurrence of its key to join
/// that row (owner decision on #55: 10 s).
pub const COLLAPSE_WINDOW_MS: i64 = 10_000;

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
    /// The absorbed rows, in arrival order.
    members: Vec<ConnectionRow>,
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

    /// Stop every open row from taking repeats ("From now" was turned on:
    /// a row after the cut must not hide inside one from before it).
    pub fn seal(&mut self) {
        self.open.clear();
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
            out.inserted.push(row);
            return;
        }
        let key = CollapseKey::of(&row);
        let open = self.open.get(&key).cloned();
        if let Some(rep_id) = open {
            if let Some(rep) = self.reps.get_mut(&rep_id) {
                if rep.takes(&row) {
                    rep.last_seen_ms = rep.last_seen_ms.max(row.started_at_ms);
                    self.member_of.insert(row.id.clone(), rep_id);
                    rep.members.push(row);
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
                rep.members[at] = row.clone();
            } else {
                rep.members.remove(at);
                self.member_of.remove(&row.id);
                out.touch(&rep.row);
                out.reinserted.push(row.clone());
            }
            return true;
        }
        let Some(rep) = self.reps.get_mut(&row.id) else {
            return false;
        };
        if collapsible(row) && CollapseKey::of(row) == rep.key {
            rep.row = row.clone();
        } else if let Some(rep) = self.reps.remove(&row.id) {
            self.close(&rep.key, &row.id);
            for member in rep.members {
                self.member_of.remove(&member.id);
                out.reinserted.push(member);
            }
        }
        out.touch(row);
        true
    }

    fn remove(&mut self, id: String, out: &mut Out) {
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
            let mut members = rep.members.into_iter();
            if let Some(first) = members.next() {
                // The oldest repeat takes the removed row's place.
                let promoted_id = first.id.clone();
                self.member_of.remove(&promoted_id);
                let mut promoted = Representative::new(first.clone());
                promoted.last_seen_ms = rep.last_seen_ms;
                for member in members {
                    self.member_of
                        .insert(member.id.clone(), promoted_id.clone());
                    promoted.members.push(member);
                }
                if was_open {
                    self.open.insert(rep.key, promoted_id.clone());
                }
                self.reps.insert(promoted_id, promoted);
                out.reinserted.push(first);
            }
        }
        out.removed.push(id);
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
        let variants: Vec<Box<dyn Fn(&mut ConnectionRow)>> = vec![
            Box::new(|r| r.matched_rule = Some("other-rule".into())),
            Box::new(|r| r.matched_rule = None),
            Box::new(|r| r.decided_by_default = true),
            Box::new(|r| r.auto_answer = Some(AutoAnswer::FilterPaused)),
            Box::new(|r| r.process = "curl".into()),
            Box::new(|r| r.process_path = Some("/usr/bin/curl".into())),
            Box::new(|r| r.dst_host = "other.example".into()),
            Box::new(|r| r.dst_ip = "203.0.113.9".into()),
            Box::new(|r| r.dst_port = 80),
            Box::new(|r| r.protocol = "udp".into()),
            Box::new(|r| r.direction = "incoming".into()),
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
