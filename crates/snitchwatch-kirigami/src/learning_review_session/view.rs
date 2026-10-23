//! The session as JSON for QML (parsed there, as the scanner page does with
//! its report). The row fields `programLabel`, `programLabelUnverified`,
//! `programPathTail` and `programPathFull` are the ones `ProgramLabel.qml`
//! reads. Every string here came from outside the GUI or a fixed sentence;
//! QML shows all of it as plain text.

use serde_json::{json, Value};

use super::{ReviewSession, Summary};
use crate::app_names::sanitize_display;
use crate::bulk_rules::{BatchState, Progress};
use crate::learning_review::{Choice, ChoiceKind, ProgramProposal};

/// The ticks are the QML-visible name of a [`ChoiceKind`].
pub fn kind_token(kind: ChoiceKind) -> &'static str {
    match kind {
        ChoiceKind::AllowAny => "allow-any",
        ChoiceKind::AllowHosts => "allow-hosts",
        ChoiceKind::Block => "block",
        ChoiceKind::DirectoryRule => "directory",
        ChoiceKind::NonSelectable => "none",
    }
}

pub fn kind_from_token(token: &str) -> Option<ChoiceKind> {
    [
        ChoiceKind::AllowAny,
        ChoiceKind::AllowHosts,
        ChoiceKind::Block,
        ChoiceKind::DirectoryRule,
    ]
    .into_iter()
    .find(|kind| kind_token(*kind) == token)
}

fn choice_json(row: &ProgramProposal, choice: &Choice) -> Option<Value> {
    let (title, hosts, note) = match choice {
        Choice::AllowHosts(hosts) => ("Allow only these destinations", hosts.clone(), None),
        Choice::AllowAny => (
            "Allow any destination",
            Vec::new(),
            row.allow_any_warning.clone(),
        ),
        Choice::Block => ("BLOCK THIS PROGRAM", Vec::new(), row.block_warning.clone()),
        Choice::DirectoryRule(rule) => (
            "Allow any program file in this folder",
            Vec::new(),
            Some(rule.text()),
        ),
        Choice::NonSelectable(_) => return None,
    };
    Some(json!({
        "kind": kind_token(choice.kind()),
        "title": title,
        "hosts": hosts,
        "note": note,
    }))
}

fn row_json(row: &ProgramProposal) -> Value {
    let choices: Vec<Value> = row
        .choices
        .iter()
        .filter_map(|choice| choice_json(row, choice))
        .collect();
    let blocked_why = row.choices.iter().find_map(|choice| match choice {
        Choice::NonSelectable(why) => Some(why.text()),
        _ => None,
    });
    json!({
        "key": row.key,
        "programLabel": row.name.label,
        "programLabelUnverified": row.marker,
        "programPathTail": row.path_tail,
        "programPathFull": row.display,
        "flags": row.describe_flags(),
        "connections": row.connections,
        "firstSeenMs": row.first_seen_ms,
        "lastSeenMs": row.last_seen_ms,
        "distinct": row.destinations.distinct,
        "distinctExact": row.destinations.exact,
        "destinationsHidden": row.destinations.hidden,
        "top": row.destinations.top.iter()
            .map(|(host, count)| json!({"host": sanitize_display(host), "count": count}))
            .collect::<Vec<_>>(),
        "highFanOut": row.high_fan_out,
        "dualUse": row.dual_use,
        "suggested": row.selected_by_suggest,
        "choices": choices,
        "notSelectable": blocked_why,
        "alreadyPresent": row.already_present.iter()
            .map(|c| kind_token(c.kind()))
            .collect::<Vec<_>>(),
    })
}

impl ReviewSession {
    /// The rows, without the ticks (those change often: `ticks_json`).
    pub fn rows_json(&self) -> String {
        Value::Array(self.proposals.iter().map(row_json).collect()).to_string()
    }

    /// `{ "<row key>": "<kind token>" }`.
    pub fn ticks_json(&self) -> String {
        let map: serde_json::Map<String, Value> = self
            .ticks
            .iter()
            .map(|(key, kind)| (key.clone(), json!(kind_token(*kind))))
            .collect();
        Value::Object(map).to_string()
    }

    pub fn notices_json(&self) -> String {
        let n = &self.notices;
        json!({
            "programs": n.programs,
            "incomplete": n.incomplete,
            "dropped": n.dropped,
            "droppedFlagged": n.dropped_flagged,
            "classAFull": n.class_a_full,
            "dirsFull": n.dirs_full,
            "dirUnlistedConnections": n.dir_unlisted_connections,
            "rulesRead": n.rules_read,
        })
        .to_string()
    }

    /// `{ "count", "lines", "extras", "confirmed", "droppedNames" }`, or
    /// `null` before the summary is prepared.
    pub fn summary_json(&self) -> String {
        match self.summary() {
            None => Value::Null.to_string(),
            Some(Summary {
                lines,
                extras,
                confirmed,
                dropped_names,
            }) => json!({
                "count": lines.len(),
                "lines": lines,
                "extras": extras,
                "confirmed": confirmed,
                "droppedNames": dropped_names,
            })
            .to_string(),
        }
    }
}

/// A running or finished batch: `{ "state", "summary", "rows": [{ "label",
/// "status" }] }`. The labels carry program names and paths from outside.
pub fn progress_json(progress: &Progress) -> String {
    let state = match &progress.state {
        BatchState::Running => "running",
        BatchState::Finished => "finished",
        BatchState::Cancelled => "cancelled",
        BatchState::Aborted(_) => "aborted",
    };
    json!({
        "state": state,
        "summary": progress.summary(),
        "rows": progress.rows.iter()
            .map(|row| json!({"label": row.label, "status": row.status.text()}))
            .collect::<Vec<_>>(),
    })
    .to_string()
}
