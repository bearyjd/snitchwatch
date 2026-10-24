//! Allowlisted values and request controls; selectors must never enter artifacts or logs.

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

pub const MAX_TIMESTAMP_MS: i64 = 253_402_300_799_999;
pub const MAX_INSPECTED_ROWS: usize = 10_000;
pub const MAX_REQUIRED_FIELD_BYTES: usize = 4096;
pub const MAX_COPIED_INPUT_BYTES: usize = 8_388_608;
pub const MAX_EMITTED_GROUPS: usize = 2000;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportFilters {
    pub window: Option<TimeWindow>,
    pub app: Option<AppSelector>,
    pub verdicts: Option<Vec<ExportVerdict>>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimeWindow {
    pub from_ms: i64,
    pub to_ms: i64,
}

#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum AppSelector {
    #[serde(rename = "path")]
    Path { value: String },
    #[serde(rename = "missingPath")]
    MissingPath,
}

impl<'de> Deserialize<'de> for AppSelector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An empty struct variant rejects surplus fields; serde's unit variant does not.
        #[derive(Deserialize)]
        #[serde(tag = "kind", deny_unknown_fields)]
        enum Input {
            #[serde(rename = "path")]
            Path { value: String },
            #[serde(rename = "missingPath")]
            MissingPath {},
        }
        Ok(match Input::deserialize(deserializer)? {
            Input::Path { value } => Self::Path { value },
            Input::MissingPath {} => Self::MissingPath,
        })
    }
}

impl ExportFilters {
    pub fn validate(&self) -> Result<(), ExportError> {
        if self.window.is_some_and(|window| {
            !valid_timestamp(window.from_ms)
                || !valid_timestamp(window.to_ms)
                || window.from_ms >= window.to_ms
        }) {
            return Err(ExportError::InvalidRequest);
        }
        if matches!(&self.app, Some(AppSelector::Path { value }) if value.len() > MAX_REQUIRED_FIELD_BYTES)
        {
            return Err(ExportError::InvalidRequest);
        }
        if let Some(verdicts) = &self.verdicts {
            if verdicts.len() > 5
                || verdicts
                    .iter()
                    .enumerate()
                    .any(|(index, verdict)| verdicts[..index].contains(verdict))
            {
                return Err(ExportError::InvalidRequest);
            }
        }
        Ok(())
    }
}

pub(crate) fn valid_timestamp(value: i64) -> bool {
    (0..=MAX_TIMESTAMP_MS).contains(&value)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ExportVerdict {
    Allow,
    Deny,
    Blocklist,
    Pending,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ExportProtocol {
    Tcp,
    Udp,
    Icmp,
    Icmpv6,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ExportRuleClass {
    Default,
    Interactive,
    Blocklist,
    Profile,
    Packaged,
    Curated,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ExportAutoAnswer {
    None,
    FilterPaused,
    NoAnswer,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ObservedRange {
    pub first_ms: i64,
    pub last_ms: i64,
}

/// Source-stage facts only: these counts say nothing about unexamined selection matches.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceCoverage {
    pub retained: usize,
    pub inspected: usize,
    pub unexamined: usize,
    pub accepted_snapshot: usize,
    pub omitted_invalid_timestamp: usize,
    pub omitted_required_field: usize,
    pub omitted_copy_budget: usize,
    pub copied_input_bytes: usize,
    pub examined_range: Option<ObservedRange>,
}

#[derive(Clone)]
pub struct ExportControl {
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
}

impl ExportControl {
    pub fn check(&self) -> Result<(), ExportError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(ExportError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(ExportError::TimedOut)
        } else {
            Ok(())
        }
    }
}

/// Static refusal codes: never attach source values or diagnostic strings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExportError {
    InvalidRequest,
    UnsupportedVersion,
    Busy,
    Cancelled,
    TimedOut,
    Unavailable,
    Internal,
    Collision,
    TooLarge,
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
            Self::Collision => "collision",
            Self::TooLarge => "too_large",
        })
    }
}

impl std::error::Error for ExportError {}

/// Only allowlisted output fields. Raw source and request selectors cannot be serialized here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDocument {
    pub(super) format: &'static str,
    pub(super) schema_version: u8,
    pub(super) redaction_version: u8,
    pub(super) captured_at_ms: i64,
    pub(super) time_semantics: &'static str,
    pub(super) requested_window: Option<TimeWindow>,
    pub(super) filter_kinds: FilterKinds,
    pub(super) counts: ExportCounts,
    pub(super) coverage: ExportCoverage,
    pub(super) unavailable: &'static [&'static str],
    pub(super) human_summary: &'static str,
    pub(super) groups: Vec<ExportGroup>,
    pub(super) dictionary: ExportDictionary,
    pub(super) analysis_prompt: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FilterKinds {
    pub time_window: bool,
    pub application: bool,
    pub verdicts: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportCounts {
    pub retained: usize,
    pub inspected: usize,
    pub accepted_snapshot: usize,
    pub selected: usize,
    pub emitted: usize,
    pub emitted_groups: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportCoverage {
    pub scope: &'static str,
    pub completeness: &'static str,
    pub unexamined_retained: usize,
    pub examined_timestamp_range: Option<ObservedRange>,
    pub row_omissions: RowOmissions,
    pub group_omissions: GroupOmissions,
    pub cache_evictions: &'static str,
    pub cache_restarts: &'static str,
    pub upstream_gaps: &'static str,
    pub missed_events: &'static str,
    pub selection_beyond_snapshot: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RowOmissions {
    pub inspection_limit: usize,
    pub field_too_long: usize,
    pub input_budget: usize,
    pub invalid_timestamp: usize,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GroupOmissions {
    pub group_limit: OmittedGroups,
    pub artifact_limit: OmittedGroups,
    pub wire_limit: OmittedGroups,
}

#[derive(Default, Serialize)]
pub(super) struct OmittedGroups {
    pub groups: usize,
    pub observations: usize,
}

#[derive(Clone, Copy)]
pub enum GroupOmissionReason {
    ArtifactLimit,
    WireLimit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportGroup {
    pub id: String,
    pub app_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_basename: Option<String>,
    pub destination_ref: String,
    pub port: u16,
    pub protocol: ExportProtocol,
    pub verdict: ExportVerdict,
    pub rule_class: ExportRuleClass,
    pub auto_answer: ExportAutoAnswer,
    pub count: usize,
    pub first_observed_at_ms: i64,
    pub last_observed_at_ms: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportDictionary {
    pub observation: &'static str,
    pub group: &'static str,
    pub pseudonyms: &'static str,
    pub app_basename: &'static str,
    pub protocol: &'static str,
    pub verdict: &'static str,
    pub rule_class: &'static str,
    pub auto_answer: &'static str,
    pub timestamps: &'static str,
    pub coverage: &'static str,
    pub omissions: &'static str,
    pub unavailable: &'static str,
}
