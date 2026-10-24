//! Private projection. Raw identities intentionally implement neither Debug nor Serialize.

use super::dto::*;
use crate::ws_messages::ConnectionRow;

pub struct Snapshot {
    pub(crate) rows: Vec<SnapshotRow>,
    pub(crate) captured_at_ms: i64,
    pub(crate) source: SourceCoverage,
}

pub(crate) struct SnapshotRow {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) app: AppIdentity,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) destination: String,
    #[expect(dead_code, reason = "Task 2 builder consumes private projected fields")]
    pub(crate) dst_port: u16,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) started_at_ms: i64,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) protocol: ExportProtocol,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) verdict: ExportVerdict,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) rule_class: ExportRuleClass,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Task 2 builder consumes private projected fields")
    )]
    pub(crate) auto_answer: ExportAutoAnswer,
}

pub(crate) enum AppIdentity {
    Path {
        #[expect(dead_code, reason = "Task 2 builder consumes private path identity")]
        value: String,
    },
    MissingPath {
        #[cfg_attr(
            not(test),
            expect(
                dead_code,
                reason = "Task 2 builder consumes private missing-path identity"
            )
        )]
        process: String,
    },
}

impl Snapshot {
    /// Only aggregate, source-stage coverage is public; private identities stay inside this module tree.
    pub fn source(&self) -> &SourceCoverage {
        &self.source
    }

    pub fn captured_at_ms(&self) -> i64 {
        self.captured_at_ms
    }
}

/// Capture while the caller holds the cache mutex. Filtering, canonicalization,
/// hashing, sorting and serialization belong to the later, unlocked build stage.
pub fn capture(
    rows: &[ConnectionRow],
    captured_at_ms: i64,
    control: &ExportControl,
) -> Result<Snapshot, ExportError> {
    control.check()?;
    if !valid_timestamp(captured_at_ms) {
        return Err(ExportError::InvalidRequest);
    }
    let mut snapshot = Snapshot {
        rows: Vec::with_capacity(rows.len().min(MAX_INSPECTED_ROWS)),
        captured_at_ms,
        source: SourceCoverage {
            retained: rows.len(),
            unexamined: rows.len().saturating_sub(MAX_INSPECTED_ROWS),
            ..SourceCoverage::default()
        },
    };
    for row in rows.iter().rev().take(MAX_INSPECTED_ROWS) {
        control.check()?;
        // Exhaustive classification of all 17 source fields. New fields require review.
        let ConnectionRow {
            id: _, // Raw row identifier: excluded, never copied or budgeted.
            process,
            process_path,
            dst_host,
            dst_ip,
            dst_port,
            protocol,
            direction: _, // Unsupported source direction: excluded.
            action,
            bytes_sent: _, // Placeholder byte counters: excluded.
            bytes_received: _,
            started_at_ms,
            matched_rule,
            auto_answer,
            answer_deadline_ms: _, // Interactive prompt deadline: excluded.
            deferred,
            decided_by_default,
        } = row;
        snapshot.source.inspected += 1;
        if !valid_timestamp(*started_at_ms) {
            snapshot.source.omitted_invalid_timestamp += 1;
            continue;
        }
        // Range covers all valid examined times, including rows omitted below.
        match &mut snapshot.source.examined_range {
            Some(range) => {
                range.first_ms = range.first_ms.min(*started_at_ms);
                range.last_ms = range.last_ms.max(*started_at_ms);
            }
            range @ None => {
                *range = Some(ObservedRange {
                    first_ms: *started_at_ms,
                    last_ms: *started_at_ms,
                })
            }
        }
        let app_value = process_path.as_deref().unwrap_or(process.as_str());
        // The translator sometimes puts the IP in dst_host. Store one effective
        // value; the unlocked identity stage canonicalizes IP spellings uniformly.
        let destination = if dst_host.is_empty() {
            dst_ip.as_str()
        } else {
            dst_host.as_str()
        };
        let required = [
            app_value,
            destination,
            protocol.as_str(),
            action.as_deref().unwrap_or(""),
            matched_rule.as_deref().unwrap_or(""),
        ];
        if required
            .iter()
            .any(|value| value.len() > MAX_REQUIRED_FIELD_BYTES)
        {
            snapshot.source.omitted_required_field += 1;
            continue;
        }
        let copied_total = app_value
            .len()
            .checked_add(destination.len())
            .and_then(|additional| {
                checked_copy_total(snapshot.source.copied_input_bytes, additional)
            });
        let Some(copied_total) = copied_total else {
            snapshot.source.omitted_copy_budget += 1;
            continue;
        };
        // Only these two identity strings are copied. Enum-classified strings
        // and unused alternatives never consume the copied-input budget.
        snapshot.rows.push(SnapshotRow {
            app: match process_path {
                Some(_) => AppIdentity::Path {
                    value: app_value.to_owned(),
                },
                None => AppIdentity::MissingPath {
                    process: app_value.to_owned(),
                },
            },
            destination: destination.to_owned(),
            dst_port: *dst_port,
            started_at_ms: *started_at_ms,
            protocol: match protocol.as_str() {
                "tcp" => ExportProtocol::Tcp,
                "udp" => ExportProtocol::Udp,
                "icmp" => ExportProtocol::Icmp,
                "icmpv6" => ExportProtocol::Icmpv6,
                _ => ExportProtocol::Unknown,
            },
            verdict: match action.as_deref() {
                Some("allow") => ExportVerdict::Allow,
                Some("deny") => ExportVerdict::Deny,
                Some("blocklist") => ExportVerdict::Blocklist,
                None if !deferred && !decided_by_default => ExportVerdict::Pending,
                _ => ExportVerdict::Unknown,
            },
            rule_class: classify_rule(matched_rule.as_deref(), *decided_by_default),
            auto_answer: match auto_answer {
                None => ExportAutoAnswer::None,
                Some(crate::ws_messages::AutoAnswer::FilterPaused) => {
                    ExportAutoAnswer::FilterPaused
                }
                Some(crate::ws_messages::AutoAnswer::NoAnswer) => ExportAutoAnswer::NoAnswer,
                Some(crate::ws_messages::AutoAnswer::Unknown) => ExportAutoAnswer::Unknown,
            },
        });
        snapshot.source.copied_input_bytes = copied_total;
    }
    control.check()?;
    snapshot.source.accepted_snapshot = snapshot.rows.len();
    Ok(snapshot)
}

pub(crate) fn checked_copy_total(current: usize, additional: usize) -> Option<usize> {
    current
        .checked_add(additional)
        .filter(|total| *total <= MAX_COPIED_INPUT_BYTES)
}

fn classify_rule(name: Option<&str>, decided_by_default: bool) -> ExportRuleClass {
    use crate::rule_name::{
        is_reserved_blocklist_name, is_reserved_curated_name, is_reserved_packaged_name,
        is_reserved_profile_name,
    };
    if decided_by_default {
        return ExportRuleClass::Default;
    }
    match name {
        None => ExportRuleClass::Unknown,
        Some(name) if is_reserved_blocklist_name(name) => ExportRuleClass::Blocklist,
        Some(name) if is_reserved_profile_name(name) => ExportRuleClass::Profile,
        Some(name) if is_reserved_packaged_name(name) => ExportRuleClass::Packaged,
        Some(name) if is_reserved_curated_name(name) => ExportRuleClass::Curated,
        Some(name) if crate::translator::verdict::is_answer_name(name) => {
            ExportRuleClass::Interactive
        }
        Some(_) => ExportRuleClass::Other,
    }
}
