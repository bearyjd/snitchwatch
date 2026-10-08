//! Rule import/export (roadmap P2.7) without Qt: the bounded import read,
//! the owner-only export write, and the preview's grouping and default
//! ticks. [`crate::rules_io_controller`] binds this to QML.
//!
//! The bridge is authoritative (it parses and checks every rule again);
//! these checks only fail fast and keep the UI thread safe: the file must be
//! a regular file (opening a FIFO would block), at most
//! [`MAX_IMPORT_FILE_BYTES`], and JSON; the request built from it must fit
//! the bridge's client message cap, which re-serializing can change.

use serde::Serialize;
use snitchwatch_bridge::rule_io::{ImportItem, ImportKind, ImportOutcome, OmittedCounts};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Largest import file read, the bridge's document cap.
pub const MAX_IMPORT_FILE_BYTES: usize = snitchwatch_bridge::rule_io::MAX_DOCUMENT_BYTES;

/// Why a file wasn't read or written. Fixed text: nothing echoes the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileError {
    NotLocal,
    NotAFile,
    TooLarge,
    Unreadable,
    NotJson,
    NotWritten,
    NotPrivate,
}

impl FileError {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotLocal => "Choose a file on this computer.",
            Self::NotAFile => "That isn't a regular file.",
            Self::TooLarge => "This file is too large to import (the limit is 960 KiB).",
            Self::Unreadable => "The file couldn't be read.",
            Self::NotJson => "This file isn't valid JSON, so it isn't a Snitchwatch rules file.",
            Self::NotWritten => "The file couldn't be saved there.",
            Self::NotPrivate => {
                "The file couldn't be made readable only by you, so the export wasn't saved there."
            }
        }
    }
}

/// Read an import file: a regular file of at most [`MAX_IMPORT_FILE_BYTES`]
/// holding JSON.
pub fn read_import_file(path: &Path) -> Result<serde_json::Value, FileError> {
    // Checked before opening: opening a FIFO blocks until a writer appears.
    let meta = std::fs::metadata(path).map_err(|_| FileError::Unreadable)?;
    if !meta.is_file() {
        return Err(FileError::NotAFile);
    }
    if meta.len() > MAX_IMPORT_FILE_BYTES as u64 {
        return Err(FileError::TooLarge);
    }
    let file = File::open(path).map_err(|_| FileError::Unreadable)?;
    // The path may have been swapped since the check.
    if !file.metadata().is_ok_and(|opened| opened.is_file()) {
        return Err(FileError::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FileError::Unreadable)?;
    if bytes.len() > MAX_IMPORT_FILE_BYTES {
        return Err(FileError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| FileError::NotJson)
}

/// The `PreviewRulesImport` message for `document`, refused when its JSON
/// would exceed the bridge's client message cap (the bridge would drop it).
pub fn preview_request(document: serde_json::Value) -> Result<ClientMessage, FileError> {
    let message = ClientMessage::PreviewRulesImport { document };
    let bytes = serde_json::to_vec(&message).map_err(|_| FileError::NotJson)?;
    if bytes.len() > snitchwatch_bridge::ws_server::MAX_CLIENT_MESSAGE_BYTES {
        return Err(FileError::TooLarge);
    }
    Ok(message)
}

/// Save an export readable only by its owner: it names programs and hosts.
/// An existing file is made private before it is truncated and rewritten.
pub fn write_export_file(path: &Path, contents: &str) -> Result<(), FileError> {
    if std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) {
        return Err(FileError::NotWritten);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| FileError::NotWritten)?;
    // `mode` applies only to a new file. Some file systems (a portal's
    // FUSE) may refuse chmod; then the file must already be private.
    if file.set_permissions(Permissions::from_mode(0o600)).is_err() {
        let mode = file
            .metadata()
            .map_err(|_| FileError::NotWritten)?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(FileError::NotPrivate);
        }
    }
    file.set_len(0)
        .and_then(|()| file.write_all(contents.as_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(|_| FileError::NotWritten)
}

const WEAKENS: &str = "This replaces a stricter rule with a looser one, so it starts unticked.";
const PRECEDENCE_ALLOW: &str =
    "This allow overrides other rules, including denies, so it starts unticked.";
const ALL_APPS_ALLOW: &str = "This allow applies to every app, so it starts unticked.";

/// Why a change starts unticked, or `None` when it starts ticked.
pub fn caution(item: &ImportItem) -> Option<&'static str> {
    if !matches!(item.kind, ImportKind::Add | ImportKind::Replace) {
        return None;
    }
    let allow = item.action == "allow";
    if item.weakens {
        Some(WEAKENS)
    } else if allow && item.precedence {
        Some(PRECEDENCE_ALLOW)
    } else if allow && item.applies_to_all_apps {
        Some(ALL_APPS_ALLOW)
    } else {
        None
    }
}

/// Adds and replaces start ticked unless they loosen (see [`caution`]).
pub fn default_ticked(item: &ImportItem) -> bool {
    matches!(item.kind, ImportKind::Add | ImportKind::Replace) && caution(item).is_none()
}

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

/// What a change installs, one plain line each.
fn details(item: &ImportItem) -> Vec<String> {
    let mut lines = vec![format!(
        "Action: {}{}",
        item.action,
        if item.enabled { "" } else { " (disabled)" }
    )];
    lines.push(match item.duration.as_str() {
        "always" => "Lasts: always".to_string(),
        _ => "Lasts: until the firewall restarts".to_string(),
    });
    lines.extend(item.conditions.iter().map(|c| format!("Condition: {c}")));
    if !item.description.is_empty() {
        lines.push(format!("Description: {}", item.description));
    }
    if item.nolog {
        lines.push("Matches aren't logged".to_string());
    }
    if !item.changed_fields.is_empty() {
        lines.push(format!("Changes: {}", item.changed_fields.join(", ")));
    }
    lines
}

/// One preview row as `RulesImportSheet.qml` renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRow {
    pub name: String,
    pub display_name: String,
    pub ticked: bool,
    pub caution: String,
    pub badges: Vec<String>,
    pub details: Vec<String>,
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
        ticked: default_ticked(item),
        caution: caution(item).unwrap_or_default().to_string(),
        badges: if change {
            badges(item).into_iter().map(str::to_string).collect()
        } else {
            Vec::new()
        },
        details: if change { details(item) } else { Vec::new() },
        problems: item
            .problems
            .iter()
            .map(|p| format!("{} ({})", p.reason, p.path))
            .collect(),
    }
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

fn count(n: u32, what: &str) -> Option<String> {
    (n > 0).then(|| format!("{n} {what}"))
}

/// What an export holds and left out; warns when the file is too large to
/// import back.
pub fn export_summary(rules: usize, omitted: &OmittedCounts, bytes: usize) -> String {
    let mut summary = if rules == 1 {
        "Exported 1 rule.".to_string()
    } else {
        format!("Exported {rules} rules.")
    };
    let left_out: Vec<String> = [
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
    if total > 0 {
        let rules_word = if total == 1 {
            "rule wasn't"
        } else {
            "rules weren't"
        };
        summary.push_str(&format!(
            " {total} {rules_word} exported: {}.",
            left_out.join(", ")
        ));
    }
    if bytes > MAX_IMPORT_FILE_BYTES {
        summary.push_str(
            " This file is too large to import back into Snitchwatch (the limit is 960 KiB).",
        );
    }
    summary
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
