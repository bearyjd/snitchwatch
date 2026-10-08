//! Rule export and import documents (roadmap P2.7). Pure: no I/O, no daemon.
//!
//! A document is a versioned envelope around rules in #48's wire shape
//! ([`crate::rule_wire`]), minus the display-only fields:
//!
//! ```json
//! { "format": "snitchwatch.rules", "version": 1, "exportedAtUnixMs": 1791400000000,
//!   "source": {}, "rules": [ { "name": "…", "enabled": true, "action": "deny", … } ] }
//! ```
//!
//! **Export** ([`export`]) writes only what an import would accept back:
//! `always` and `until restart` user rules. Rules the bridge owns
//! (blocklists, curated defaults), `once` and timed rules, and rules
//! `rule_policy::validate_user_rule` refuses are left out and counted
//! ([`OmittedCounts`]).
//!
//! **Import** is untrusted file content. [`parse_document`] checks the
//! envelope (`deny_unknown_fields`, version 1, at most
//! [`MAX_SNAPSHOT_RULES`] rules); [`preview`] runs every rule through
//! `rule_from_wire` and the `Import` policy profile and classifies it
//! against the cached rules. The daemon replaces a same-name rule on
//! `CHANGE_RULE` (`Loader.Replace`, no `-2` suffix), so a same-name rule is a
//! replace, never a second copy. Nothing here deletes a rule.
//!
//! Every error is fixed text: nothing echoes the file.

use crate::cache::rules::{RulesCache, MAX_SNAPSHOT_RULES};
use crate::rule_policy::{validate_user_rule, PolicyProfile, RuleProblem};
use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::Rule;

mod preview;
pub use preview::{
    check_rules, classify, preview, CheckedRule, ImportItem, ImportKind, ImportPreview,
    DUPLICATE_NAME,
};

pub const FORMAT: &str = "snitchwatch.rules";
pub const VERSION: u32 = 1;
/// Room the `PreviewRulesImport` envelope needs around a document inside one
/// client WebSocket message.
pub const ENVELOPE_SLACK_BYTES: usize = 64 * 1024;
/// Largest document, in bytes of JSON. Bounded by the bridge's existing
/// client message cap ([`crate::ws_server::MAX_CLIENT_MESSAGE_BYTES`]), not
/// raised for imports.
pub const MAX_DOCUMENT_BYTES: usize =
    crate::ws_server::MAX_CLIENT_MESSAGE_BYTES - ENVELOPE_SLACK_BYTES;

/// A version-1 document. Unknown keys are refused, so a later version can
/// add some (blocklist subscriptions, profiles) without an old bridge
/// silently dropping them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Document {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub exported_at_unix_ms: u64,
    #[serde(default)]
    pub source: Source,
    pub rules: Vec<serde_json::Value>,
}

/// Where a document came from. Informational only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
}

/// Why a document was refused as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentError {
    TooLarge,
    NotJson,
    NotRulesFile,
    Newer,
    UnsupportedVersion,
    Malformed,
    TooManyRules,
}

impl DocumentError {
    pub fn describe(self) -> &'static str {
        match self {
            Self::TooLarge => {
                "This file is too large to import. Snitchwatch imports files of up to 960 KiB, \
                 which is about 1,000 to 2,000 rules."
            }
            Self::NotJson => "This file isn't valid JSON.",
            Self::NotRulesFile => "This isn't a Snitchwatch rules file.",
            Self::Newer => "This file was made by a newer Snitchwatch.",
            Self::UnsupportedVersion => "This file's format version isn't supported.",
            Self::Malformed => {
                "This Snitchwatch rules file has missing or unexpected fields, so it wasn't read."
            }
            Self::TooManyRules => "This file has more than 10,000 rules.",
        }
    }
}

/// The cache has no daemon rule list yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportUnavailable;

impl ExportUnavailable {
    pub const REASON: &'static str = "Rules haven't loaded from the firewall yet.";
}

/// The cache has no daemon rule list to compare an import against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewUnavailable;

impl PreviewUnavailable {
    pub const REASON: &'static str =
        "Rules haven't loaded from the firewall yet, so the import can't be compared.";
}

/// Rules left out of an export, by why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OmittedCounts {
    /// `once` rules: they answer one connection and are never stored.
    pub once: u32,
    /// Timed rules (`5m`…): they'd expire on a different schedule.
    pub timed: u32,
    /// Blocklist and curated default rules, which come back from their own
    /// sources.
    pub managed: u32,
    /// Names Snitchwatch won't send back to the firewall.
    pub unsupported_name: u32,
    /// Conditions or fields the import would refuse.
    pub unsupported_rule: u32,
}

/// What happened to one rule of an import apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ImportOutcome {
    /// The daemon answered OK.
    Applied,
    /// The daemon answered ERROR; its text, sanitized for display.
    Rejected { reason: String },
    /// Sent, but no answer in time: it may or may not be in place.
    NoAnswer,
    /// Never sent (the firewall was busy or went away).
    NotSent { reason: String },
    /// The bridge refused to send it.
    Refused { reason: String },
}

/// An export and what it left out.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub document: Document,
    pub omitted: OmittedCounts,
}

/// One rule as a document carries it: #48's wire shape, fields named
/// explicitly so a display field added to `rule_to_wire` can't leak in.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedRule<'a> {
    name: &'a str,
    enabled: bool,
    action: &'a str,
    duration: &'a str,
    description: &'a str,
    precedence: bool,
    nolog: bool,
    operator: serde_json::Value,
}

/// `rule` in the document's rule shape.
pub fn export_rule(rule: &Rule) -> serde_json::Value {
    let exported = ExportedRule {
        name: &rule.name,
        enabled: rule.enabled,
        action: &rule.action,
        duration: &rule.duration,
        description: &rule.description,
        precedence: rule.precedence,
        nolog: rule.nolog,
        operator: rule
            .operator
            .as_ref()
            .map(crate::rule_wire::operator_to_wire)
            .unwrap_or(serde_json::Value::Null),
    };
    serde_json::to_value(exported).unwrap_or(serde_json::Value::Null)
}

/// Export the cached user rules an import would accept, in name order.
pub fn export(cache: &RulesCache, now_unix_ms: u64) -> Result<Export, ExportUnavailable> {
    let rules = cache.rules().ok_or(ExportUnavailable)?;
    let mut omitted = OmittedCounts::default();
    let mut exported = Vec::new();
    for rule in rules.values() {
        let counter = if is_bridge_owned(&rule.name) {
            &mut omitted.managed
        } else if crate::rule_name::validate_rule_name(&rule.name).is_err() {
            &mut omitted.unsupported_name
        } else if rule.duration == "once" {
            &mut omitted.once
        } else if !matches!(rule.duration.as_str(), "always" | "until restart") {
            &mut omitted.timed
        } else if validate_user_rule(rule, PolicyProfile::Import).is_err() {
            &mut omitted.unsupported_rule
        } else {
            exported.push(export_rule(rule));
            continue;
        };
        *counter += 1;
    }
    Ok(Export {
        document: Document {
            format: FORMAT.to_string(),
            version: VERSION,
            exported_at_unix_ms: now_unix_ms,
            source: Source::default(),
            rules: exported,
        },
        omitted,
    })
}

/// Check a previewed rule again just before it is sent (defence in depth):
/// back to the document's rule shape, through `rule_from_wire`, then the
/// `Import` profile.
pub fn check_rule_for_apply(rule: &Rule) -> Result<Rule, Vec<RuleProblem>> {
    let reparsed = crate::rule_wire::rule_from_wire(&export_rule(rule)).map_err(|reason| {
        vec![RuleProblem {
            path: "rule".into(),
            reason,
        }]
    })?;
    validate_user_rule(&reparsed, PolicyProfile::Import)?;
    Ok(reparsed)
}

/// Rules Snitchwatch installs itself: blocklists and curated defaults.
fn is_bridge_owned(name: &str) -> bool {
    crate::rule_name::is_reserved_blocklist_name(name)
        || name.starts_with(crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX)
}

/// Parse a document from file text, refusing an oversized one before any
/// JSON parsing.
pub fn parse_document_text(text: &str) -> Result<Document, DocumentError> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(DocumentError::TooLarge);
    }
    let value = serde_json::from_str(text).map_err(|_| DocumentError::NotJson)?;
    parse_document(value)
}

/// Check a document's envelope. Its rules are checked by [`preview`].
pub fn parse_document(value: serde_json::Value) -> Result<Document, DocumentError> {
    // Format and version first, so a newer file says so rather than
    // tripping over a key this version doesn't know.
    let format = value.get("format").and_then(|f| f.as_str());
    if format != Some(FORMAT) {
        return Err(DocumentError::NotRulesFile);
    }
    match value.get("version").and_then(|v| v.as_u64()) {
        Some(1) => {}
        Some(v) if v > 1 => return Err(DocumentError::Newer),
        _ => return Err(DocumentError::UnsupportedVersion),
    }
    let document: Document = serde_json::from_value(value).map_err(|_| DocumentError::Malformed)?;
    if document.rules.len() > MAX_SNAPSHOT_RULES {
        return Err(DocumentError::TooManyRules);
    }
    Ok(document)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod protocol_tests;
