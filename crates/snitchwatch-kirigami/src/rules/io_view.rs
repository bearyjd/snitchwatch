//! What the import preview sheet and the Rules page say (roadmap P2.7),
//! without Qt: the preview's rows grouped by kind, the export and result
//! texts, and progress batched for QML.
//!
//! The bridge decides each row's default tick and writes its cautions
//! (`rule_io` preview): this only lays them out. Every string is plain
//! text for `Text.PlainText` labels.

use super::io::MAX_IMPORT_FILE_BYTES;
use serde::Serialize;
use snitchwatch_bridge::rule_io::{
    Document, ImportItem, ImportKind, ImportOutcome, OmittedCounts, PreviousRule,
};
use snitchwatch_bridge::ws_messages::ServerMessage;
use std::collections::BTreeMap;

pub fn badges(item: &ImportItem) -> Vec<&'static str> {
    let mut badges = Vec::new();
    if item.applies_to_all_apps {
        badges.push("Applies to all apps");
    }
    if item.precedence {
        badges.push("Overrides other rules");
    }
    if !item.persists {
        badges.push("Lost when the firewall restarts");
    }
    badges
}

fn lasting(duration: &str) -> &'static str {
    match duration {
        "always" => "Lasts: always",
        _ => "Lasts: until the firewall restarts",
    }
}

/// One rule's content, one plain line each.
fn rule_lines(
    action: &str,
    enabled: bool,
    duration: &str,
    conditions: &[String],
    description: &str,
    nolog: bool,
) -> Vec<String> {
    let mut lines = vec![format!(
        "Action: {action}{}",
        if enabled { "" } else { " (off)" }
    )];
    lines.push(lasting(duration).to_string());
    lines.extend(conditions.iter().map(|c| format!("Condition: {c}")));
    if !description.is_empty() {
        lines.push(format!("Description: {description}"));
    }
    if nolog {
        lines.push("Matches aren't logged".to_string());
    }
    lines
}

/// What a change installs.
fn details(item: &ImportItem) -> Vec<String> {
    let mut lines = rule_lines(
        &item.action,
        item.enabled,
        &item.duration,
        &item.conditions,
        &item.description,
        item.nolog,
    );
    if !item.changed_fields.is_empty() {
        lines.push(format!("Changes: {}", item.changed_fields.join(", ")));
    }
    lines
}

/// The rule a replace overwrites, as it is now.
fn previous_lines(previous: &PreviousRule) -> Vec<String> {
    rule_lines(
        &previous.action,
        previous.enabled,
        &previous.duration,
        &previous.conditions,
        &previous.description,
        previous.nolog,
    )
}

/// One preview row as `RulesImportSheet.qml` renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRow {
    pub name: String,
    pub display_name: String,
    pub ticked: bool,
    /// Why it starts unticked; empty when it starts ticked.
    pub caution: String,
    pub badges: Vec<String>,
    pub details: Vec<String>,
    /// For a replace, the rule it overwrites.
    pub previous: Vec<String>,
    pub problems: Vec<String>,
}

/// The preview's sections, each in file order.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PreviewView {
    pub add: Vec<PreviewRow>,
    pub replace: Vec<PreviewRow>,
    pub unchanged: Vec<PreviewRow>,
    pub refused: Vec<PreviewRow>,
}

fn row(item: &ImportItem) -> PreviewRow {
    let change = matches!(item.kind, ImportKind::Add | ImportKind::Replace);
    PreviewRow {
        name: item.name.clone(),
        display_name: item.display_name.clone(),
        ticked: change && item.ticked,
        caution: item.cautions.join(" "),
        badges: if change {
            badges(item).into_iter().map(str::to_string).collect()
        } else {
            Vec::new()
        },
        details: if change { details(item) } else { Vec::new() },
        previous: item
            .previous
            .as_ref()
            .map(previous_lines)
            .unwrap_or_default(),
        problems: item
            .problems
            .iter()
            .map(|p| match plain_location(&p.path) {
                Some(place) => format!("{} ({place})", p.reason),
                None => p.reason.clone(),
            })
            .collect(),
    }
}

/// Where a problem is, in plain words (`None`: the whole rule).
fn plain_location(path: &str) -> Option<String> {
    let place = match path {
        "rule" => return None,
        "name" => "name".to_string(),
        "enabled" => "on or off".to_string(),
        "action" => "action".to_string(),
        "duration" => "how long it lasts".to_string(),
        "operator" => "conditions".to_string(),
        "operator.data" => "the condition's value".to_string(),
        "operator.operand" => "the condition".to_string(),
        _ => match path
            .strip_prefix("operator.list[")
            .and_then(|r| r.split_once(']'))
        {
            Some((index, rest)) => {
                let n = index.parse::<usize>().map_or(0, |i| i + 1);
                match rest {
                    ".data" => format!("condition {n}'s value"),
                    _ => format!("condition {n}"),
                }
            }
            None => "the rule".to_string(),
        },
    };
    Some(place)
}

pub fn group(items: &[ImportItem]) -> PreviewView {
    let mut view = PreviewView::default();
    for item in items {
        let section = match item.kind {
            ImportKind::Add => &mut view.add,
            ImportKind::Replace => &mut view.replace,
            ImportKind::Unchanged => &mut view.unchanged,
            ImportKind::Refused => &mut view.refused,
        };
        section.push(row(item));
    }
    view
}

pub fn outcome_text(outcome: &ImportOutcome) -> String {
    match outcome {
        ImportOutcome::Applied => "Applied".to_string(),
        ImportOutcome::Rejected { reason } => {
            format!("Refused by the firewall service: {reason}")
        }
        ImportOutcome::Refused { reason } => format!("Snitchwatch didn't send it: {reason}"),
        ImportOutcome::NoAnswer => {
            "No answer from the firewall service; it may or may not be in place.".to_string()
        }
        ImportOutcome::NotSent { reason } => reason.clone(),
    }
}

pub fn result_summary(applied: u32, rejected: u32, not_sent: u32, no_answer: u32) -> String {
    let mut parts = vec![format!("{applied} applied")];
    if rejected > 0 {
        parts.push(format!("{rejected} refused by the firewall or Snitchwatch"));
    }
    if not_sent > 0 {
        parts.push(format!("{not_sent} not sent"));
    }
    if no_answer > 0 {
        parts.push(format!(
            "{no_answer} unanswered (they may or may not be in place)"
        ));
    }
    format!("{}.", parts.join(", "))
}

fn rules(n: usize) -> String {
    if n == 1 {
        "1 rule".to_string()
    } else {
        format!("{n} rules")
    }
}

fn count(n: u32, what: &str) -> Option<String> {
    (n > 0).then(|| format!("{n} {what}"))
}

/// What an export left out, or empty.
fn left_out(omitted: &OmittedCounts) -> String {
    let parts: Vec<String> = [
        count(omitted.once, "answered once"),
        count(omitted.timed, "timed"),
        count(
            omitted.managed,
            "managed by Snitchwatch (blocklists, defaults)",
        ),
        count(
            omitted.unsupported_name,
            "with names Snitchwatch can't send back",
        ),
        count(omitted.unsupported_rule, "that an import would refuse"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let total = omitted.once
        + omitted.timed
        + omitted.managed
        + omitted.unsupported_name
        + omitted.unsupported_rule;
    match total {
        0 => String::new(),
        1 => format!(" 1 rule wasn't exported: {}.", parts.join(", ")),
        n => format!(" {n} rules weren't exported: {}.", parts.join(", ")),
    }
}

/// The export arrived; the save dialog opens next.
pub fn export_ready(count: usize, omitted: &OmittedCounts, bytes: usize) -> String {
    let mut text = format!("Ready to save {}.{}", rules(count), left_out(omitted));
    if bytes > MAX_IMPORT_FILE_BYTES {
        text.push_str(
            " This file is too large to import back into Snitchwatch: imports are limited to \
             960 KiB, about 2,000 to 3,500 rules.",
        );
    }
    text
}

pub fn export_saved(count: usize, omitted: &OmittedCounts) -> String {
    format!("Saved {}.{}", rules(count), left_out(omitted))
}

pub fn export_cancelled() -> &'static str {
    "Nothing was saved."
}

pub fn export_failed(error: &str, omitted: &OmittedCounts) -> String {
    format!("{error}{}", left_out(omitted))
}

/// The export file's text: compact JSON, so the most rules fit the import
/// cap.
pub fn export_text(document: &Document) -> String {
    serde_json::to_string(document).unwrap_or_default()
}

/// An apply's per-rule outcomes, handed to QML in batches (the controller
/// flushes on its one-second poll) rather than re-serialized per rule.
#[derive(Debug, Default)]
pub struct ProgressLog {
    results: BTreeMap<String, String>,
    dirty: bool,
}

impl ProgressLog {
    pub fn record(&mut self, name: String, text: String) {
        self.results.insert(name, text);
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.results.clear();
        self.dirty = true;
    }

    /// `{ rule name: outcome }` as JSON when something changed since the
    /// last call.
    pub fn take_json(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        serde_json::to_string(&self.results).ok()
    }
}

/// What `RulesIoController` waits for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Waiting {
    #[default]
    Nothing,
    /// An answer to this request id.
    Request(String),
    /// The progress and result of applying a preview (or the apply's
    /// refusal).
    Apply {
        preview_id: String,
        request_id: String,
    },
}

/// Whether `message` answers what `waiting` waits for: its request id, or
/// the applying preview's progress and result.
pub fn awaits(waiting: &Waiting, message: &ServerMessage) -> bool {
    match (waiting, message) {
        (
            Waiting::Request(id),
            ServerMessage::RulesExport { request_id, .. }
            | ServerMessage::RulesExportUnavailable { request_id, .. }
            | ServerMessage::RulesImportPreview { request_id, .. }
            | ServerMessage::RulesImportRefused { request_id, .. },
        ) => request_id == id,
        (
            Waiting::Apply { request_id, .. },
            ServerMessage::RulesImportRefused { request_id: id, .. },
        ) => id == request_id,
        (
            Waiting::Apply { preview_id, .. },
            ServerMessage::RulesImportProgress { preview_id: id, .. }
            | ServerMessage::RulesImportResult { preview_id: id, .. },
        ) => id == preview_id,
        _ => false,
    }
}

/// The `ServerMessage`s `RulesIoController` handles.
pub fn interests_rules_io(msg: &ServerMessage) -> bool {
    matches!(
        msg,
        ServerMessage::RulesExport { .. }
            | ServerMessage::RulesExportUnavailable { .. }
            | ServerMessage::RulesImportPreview { .. }
            | ServerMessage::RulesImportRefused { .. }
            | ServerMessage::RulesImportProgress { .. }
            | ServerMessage::RulesImportResult { .. }
    )
}
