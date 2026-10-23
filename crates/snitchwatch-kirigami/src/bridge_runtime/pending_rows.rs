//! The live session's waiting prompts, kept by the runtime from the same row
//! messages the Connections model gets (prompt-slot plan Part B).
//!
//! A desktop notification is shown and acted on outside the Qt models, often
//! with the window hidden, so it can't ask `ConnectionsModel`. This keeps
//! just enough to answer "is this row still waiting, in this session?" and to
//! give an action the row's program path and destination. It follows
//! inserts, updates, removals and clears like `RowStore`, using the same
//! pending test (`connections::outcome::is_pending`), and starts empty with
//! every session.

use std::collections::HashMap;

use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};

use super::{BridgeHandles, ConnectionState};
use crate::connections::outcome::is_pending;

/// What a notification needs from a waiting row. All of it comes from
/// outside: show it only escaped (`notification_actions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRow {
    pub process: String,
    pub process_path: Option<String>,
    pub dst_host: String,
    /// The destination address (the key when `dst_host` is empty).
    pub dst_ip: String,
    pub port: u16,
    pub protocol: String,
}

impl PendingRow {
    /// What is kept of `row`.
    pub fn from_row(row: &ConnectionRow) -> Self {
        Self {
            process: row.process.clone(),
            process_path: row.process_path.clone(),
            dst_host: row.dst_host.clone(),
            dst_ip: row.dst_ip.clone(),
            port: row.dst_port,
            protocol: row.protocol.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct PendingRows {
    rows: HashMap<String, PendingRow>,
}

impl PendingRows {
    fn upsert(&mut self, row: &ConnectionRow) {
        if is_pending(row) {
            self.rows.insert(row.id.clone(), PendingRow::from_row(row));
        } else {
            self.rows.remove(&row.id);
        }
    }

    pub(super) fn observe(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::InsertConnectionRows { rows }
            | ServerMessage::UpdateConnectionRows { rows } => {
                rows.iter().for_each(|row| self.upsert(row));
            }
            ServerMessage::RemoveConnectionRows { ids }
            | ServerMessage::MoveConnetionRows { ids } => {
                for id in ids {
                    self.rows.remove(id);
                }
            }
            ServerMessage::ClearConnectionRows => self.rows.clear(),
            _ => {}
        }
    }

    pub(super) fn clear(&mut self) {
        self.rows.clear();
    }

    fn snapshot(&self) -> HashMap<String, PendingRow> {
        self.rows.clone()
    }

    fn get(&self, wire_id: &str) -> Option<&PendingRow> {
        self.rows.get(wire_id)
    }
}

/// Apply `message` to session `connection_id`'s waiting prompts, if that is
/// still the live session.
pub(super) fn observe(
    connection: &std::sync::Mutex<ConnectionState>,
    connection_id: u64,
    message: &ServerMessage,
) {
    let mut state = connection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.connected && state.connection_id == connection_id {
        state.pending_rows.observe(message);
    }
}

impl BridgeHandles {
    /// The live session and the rows now waiting in it, keyed by wire id, or
    /// `None` while disconnected. The runtime pump feeds it without loss, so
    /// it is what a sweep re-reads after the broadcast channel lagged.
    pub fn pending_rows_snapshot(&self) -> Option<(u64, HashMap<String, PendingRow>)> {
        let state = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .connected
            .then(|| (state.connection_id, state.pending_rows.snapshot()))
    }

    /// Row `wire_id` of session `connection_id`, while that session is live
    /// and the row is still waiting for an answer.
    pub fn pending_row(&self, connection_id: u64, wire_id: &str) -> Option<PendingRow> {
        let state = self
            .connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !(state.connected && state.connection_id == connection_id) {
            return None;
        }
        state.pending_rows.get(wire_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, action: Option<&str>, deferred: bool) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: action.map(str::to_owned),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred,
            decided_by_default: false,
        }
    }

    fn waiting(rows: &PendingRows) -> Vec<&str> {
        let mut ids: Vec<&str> = rows.rows.keys().map(String::as_str).collect();
        ids.sort();
        ids
    }

    #[test]
    fn it_follows_the_row_messages() {
        let mut rows = PendingRows::default();
        rows.observe(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("ask-1", None, false),
                row("ask-2", None, false),
                row("ask-3", None, false),
                row("ev-1", Some("allow"), false),
                row("ask-4", None, true),
            ],
        });
        assert_eq!(waiting(&rows), ["ask-1", "ask-2", "ask-3"]);
        assert_eq!(rows.get("ask-1").unwrap().dst_host, "example.com");
        // The full key of a waiting row, for a sweep of the rows already waiting.
        assert_eq!(
            rows.get("ask-1").unwrap(),
            &PendingRow {
                process: "curl".into(),
                process_path: Some("/usr/bin/curl".into()),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                port: 443,
                protocol: "tcp".into(),
            }
        );
        let snapshot = rows.snapshot();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot["ask-2"].port, 443);

        // Answered, put off, removed.
        rows.observe(&ServerMessage::UpdateConnectionRows {
            rows: vec![row("ask-1", Some("deny"), false), row("ask-2", None, true)],
        });
        rows.observe(&ServerMessage::RemoveConnectionRows {
            ids: vec!["ask-3".into()],
        });
        assert!(waiting(&rows).is_empty());

        rows.observe(&ServerMessage::InsertConnectionRows {
            rows: vec![row("ask-5", None, false)],
        });
        rows.observe(&ServerMessage::ClearConnectionRows);
        assert!(waiting(&rows).is_empty());
    }
}
