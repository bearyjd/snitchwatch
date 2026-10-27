//! Plain helpers for `ConnectionsModel`: flush debounce, grouped-message buffering, and row-to-role text.

use std::collections::HashSet;

use super::*;

/// How long grouped-mode mutations are coalesced before one Qt model reset
/// flushes them all. Chosen well under perception threshold but long enough
/// that a busy system's per-connection event bursts (dozens/sec) collapse to
/// ~6 resets/sec worst case instead of one per message.
pub(super) const GROUPED_FLUSH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(150);

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Seconds since `oldest_pending_started_at_ms`, or `-1` when nothing is
/// actively pending. Qt-free and unit-tested below; the Qt-facing call site
/// just supplies `store.oldest_pending_started_at_ms(now)` and that same
/// `now`.
pub(super) fn pending_age_secs(oldest_pending_started_at_ms: Option<i64>, now_ms: i64) -> i32 {
    match oldest_pending_started_at_ms {
        // Saturate rather than go negative if clock skew or a future
        // timestamp ever put `started_at_ms` ahead of `now_ms`.
        Some(started) => ((now_ms - started).max(0) / 1000) as i32,
        None => -1,
    }
}

/// Ids that arrive *pending* anywhere in a batch of buffered messages —
/// the flush-time input to the auto-select policy (Qt-free, tested below).
pub(super) fn collect_new_pending_ids(msgs: &[ServerMessage]) -> Vec<String> {
    msgs.iter()
        .flat_map(|msg| match msg {
            ServerMessage::InsertConnectionRows { rows } => rows
                .iter()
                .filter(|r| is_pending(r))
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect()
}

/// QML keeps these opaque IDs in delegates and held-open dialogs. Qualifying
/// before storage makes every verdict retain its originating service session.
pub(super) fn qualify_connection_ids(msg: &mut ServerMessage, connection_id: u64) {
    let qualify = |id: &mut String| *id = format!("{connection_id}:{id}");
    match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => {
            for row in rows {
                qualify(&mut row.id);
            }
        }
        ServerMessage::RemoveConnectionRows { ids } | ServerMessage::MoveConnetionRows { ids } => {
            for id in ids {
                qualify(id);
            }
        }
        _ => {}
    }
}

pub(super) struct BufferedConnectionMessage {
    // None is reserved for explicit injection in headless/model tests.
    pub(super) connection_id: Option<u64>,
    pub(super) message: ServerMessage,
}

pub(super) fn take_current_grouped_messages(
    buffered: Vec<BufferedConnectionMessage>,
    is_current: impl Fn(u64) -> bool,
) -> Vec<ServerMessage> {
    buffered
        .into_iter()
        .filter_map(|entry| {
            entry
                .connection_id
                .is_none_or(&is_current)
                .then_some(entry.message)
        })
        .collect()
}

/// Mirror one message's delta into the Process->Domain group tree, Qt-free
/// and incremental (see `connections::grouping` module docs) — shared by the
/// immediate and debounced apply paths.
pub(super) fn mirror_into_grouping(rust: &mut ConnectionsModelRust, msg: &ServerMessage) {
    // Every tree write reads the windows against the wall clock (phase 0b),
    // on the immediate and the buffered grouped path alike.
    rust.grouping.set_clock(verdict_windows::local_clock());
    match msg {
        ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows } => {
            for row in rows {
                rust.grouping.upsert_row(row);
            }
            // First-seen state (0c): flags only, never visibility.
            let paths = rows.iter().filter_map(|row| row.process_path.as_deref());
            rust.seen.observe(paths, now_ms());
            let ConnectionsModelRust {
                store,
                handled_counts,
                ..
            } = &mut *rust;
            let update = matches!(msg, ServerMessage::UpdateConnectionRows { .. });
            for row in rows {
                // The store ignores an update for an id it does not hold, so
                // the group must not count one either.
                if update && store.row_by_id(&row.id).is_none() {
                    continue;
                }
                handled_counts.upsert(store.filter().attention_facts(), row);
            }
        }
        ServerMessage::RemoveConnectionRows { ids } => {
            for id in ids {
                rust.grouping.remove_row(id);
                rust.handled_counts.remove(id);
            }
        }
        ServerMessage::MoveConnetionRows { ids } => {
            rust.grouping.move_to_front(ids);
        }
        ServerMessage::ClearConnectionRows => {
            rust.grouping.clear();
            // A reconnect's clear resets neither the seen set nor the flags,
            // but nothing collapses again until this session's rules arrive:
            // the previous session's may name a rule deleted meanwhile.
            rust.seen.note_clear_applied();
            rust.attention_armed = false;
            rust.store.set_attention_facts(std::sync::Arc::new(
                crate::connections::attention::AttentionFacts::default(),
            ));
            rust.handled_counts.clear();
        }
        _ => {}
    }
}

/// Whether a filtering pause answered the row (issue #78).
pub(super) fn answered_while_paused(row: &ConnectionRow) -> bool {
    row.auto_answer == Some(AutoAnswer::FilterPaused)
}

pub(super) fn answer_deadline_ms(row: &ConnectionRow) -> f64 {
    row.answer_deadline_ms.map_or(-1.0, |ms| ms as f64)
}

/// Read one role of a grouped-projection [`VisibleEntry`] into the QVariant
/// shape `data()` returns. `store` resolves leaf `Row` entries' full
/// `ConnectionRow` content (the tree only tracks ids).
/// The row's executable path, empty when the daemon reported none.
pub(super) fn process_path_text(row: &ConnectionRow) -> &str {
    row.process_path.as_deref().unwrap_or("")
}

/// Position of the pending row with the smallest `started_at_ms` (the one
/// waiting longest), skipping every id in `except`. Ties go to the smallest
/// `store_order` (the row earlier in the store, the rule `alert_row` uses),
/// then to the topmost. `None` entries (group headers) are never candidates.
pub(super) fn oldest_pending_position<'a>(
    rows: impl Iterator<Item = Option<&'a ConnectionRow>>,
    except: &HashSet<String>,
    store_order: impl Fn(&str) -> usize,
) -> Option<usize> {
    rows.enumerate()
        .filter_map(|(i, r)| r.map(|r| (i, r)))
        .filter(|(_, r)| !except.contains(&r.id) && is_pending(r))
        .min_by_key(|(_, r)| (r.started_at_ms, store_order(&r.id)))
        .map(|(i, _)| i)
}

/// The ids in a JSON array of strings; none for anything else.
pub(super) fn parse_except_ids(json: &str) -> HashSet<String> {
    serde_json::from_str::<Vec<String>>(json)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// `startedAtMs` as a `real` (epoch ms overflow a QML `int`).
pub(super) fn started_at_ms(row: &ConnectionRow) -> f64 {
    row.started_at_ms as f64
}

/// Icon for the row's process; the generic icon when the path is unknown.
pub(super) fn app_icon_for_row(row: &ConnectionRow) -> String {
    crate::app_icons::icon_for_path(process_path_text(row))
}

/// Whether the GUI answered `row` silently and it is no longer waiting.
pub(super) fn needs_rule_for_row(row: &ConnectionRow) -> bool {
    !is_pending(row) && crate::silent_mode::needs_rule(&row.id)
}

pub(super) fn grouped_entry_data(entry: &VisibleEntry, role: i32, store: &RowStore) -> QVariant {
    // Roles every tier answers the same way, whatever its shape.
    match role {
        ROLE_GROUP_LEVEL => return QVariant::from(&entry.level()),
        ROLE_GROUP_PROCESS_KEY => {
            let key = match entry {
                VisibleEntry::HostHeader { process_key, .. } => process_key.as_str(),
                _ => "",
            };
            return QVariant::from(&QString::from(key));
        }
        _ => {}
    }
    match entry {
        VisibleEntry::ProcessHeader {
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!("process:{key}"))),
            ROLE_PROCESS | ROLE_HOST | ROLE_PROTOCOL | ROLE_VERDICT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_PORT => QVariant::from(&0i32),
            ROLE_PENDING => QVariant::from(&(counts.pending > 0)),
            ROLE_DEPTH => QVariant::from(&0i32),
            ROLE_IS_GROUP_HEADER => QVariant::from(&true),
            ROLE_EXPANDED => QVariant::from(expanded),
            ROLE_GROUP_KEY => QVariant::from(&QString::from(key)),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from("")),
            ROLE_GROUP_LABEL => QVariant::from(&QString::from(label)),
            ROLE_GROUP_TOTAL => QVariant::from(&(counts.total as i32)),
            ROLE_GROUP_PENDING => QVariant::from(&(counts.pending as i32)),
            ROLE_GROUP_ALLOWED => QVariant::from(&(counts.allowed as i32)),
            ROLE_GROUP_DENIED => QVariant::from(&(counts.denied as i32)),
            ROLE_GROUP_BLOCKLISTED => QVariant::from(&(counts.blocklisted as i32)),
            ROLE_GROUP_LAST_HOUR | ROLE_GROUP_TODAY => verdict_windows::window_role(counts, role),
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY | ROLE_OUTCOME_TEXT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_ANSWERED_WHILE_PAUSED | ROLE_MAKE_RULE_OFFERED => QVariant::from(&false),
            ROLE_ANSWER_DEADLINE_MS | ROLE_STARTED_AT_MS => QVariant::from(&-1.0f64),
            ROLE_PROCESS_PATH | ROLE_DIRECTION | ROLE_APP_ICON => {
                QVariant::from(&QString::from(""))
            }
            ROLE_PROGRAM_LABEL
            | ROLE_PROGRAM_LABEL_UNVERIFIED
            | ROLE_PROGRAM_PATH_TAIL
            | ROLE_PROGRAM_PATH_FULL => program_label::role_for_program_header(key, label, role),
            _ => QVariant::default(),
        },
        VisibleEntry::DomainHeader {
            process_key,
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!("domain:{process_key}|{key}"))),
            ROLE_PROCESS | ROLE_HOST | ROLE_PROTOCOL | ROLE_VERDICT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_PORT => QVariant::from(&0i32),
            ROLE_PENDING => QVariant::from(&(counts.pending > 0)),
            ROLE_DEPTH => QVariant::from(&1i32),
            ROLE_IS_GROUP_HEADER => QVariant::from(&true),
            ROLE_EXPANDED => QVariant::from(expanded),
            ROLE_GROUP_KEY => QVariant::from(&QString::from(key)),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from(process_key)),
            ROLE_GROUP_LABEL => QVariant::from(&QString::from(label)),
            ROLE_GROUP_TOTAL => QVariant::from(&(counts.total as i32)),
            ROLE_GROUP_PENDING => QVariant::from(&(counts.pending as i32)),
            ROLE_GROUP_ALLOWED => QVariant::from(&(counts.allowed as i32)),
            ROLE_GROUP_DENIED => QVariant::from(&(counts.denied as i32)),
            ROLE_GROUP_BLOCKLISTED => QVariant::from(&(counts.blocklisted as i32)),
            ROLE_GROUP_LAST_HOUR | ROLE_GROUP_TODAY => verdict_windows::window_role(counts, role),
            ROLE_MATCHED_RULE | ROLE_MATCHED_RULE_DISPLAY | ROLE_OUTCOME_TEXT => {
                QVariant::from(&QString::from(""))
            }
            ROLE_ANSWERED_WHILE_PAUSED | ROLE_MAKE_RULE_OFFERED | ROLE_NEEDS_RULE => {
                QVariant::from(&false)
            }
            ROLE_ANSWER_DEADLINE_MS | ROLE_STARTED_AT_MS => QVariant::from(&-1.0f64),
            ROLE_PROCESS_PATH
            | ROLE_DIRECTION
            | ROLE_APP_ICON
            | ROLE_PROGRAM_PATH_TAIL
            | ROLE_PROGRAM_PATH_FULL => QVariant::from(&QString::from("")),
            ROLE_PROGRAM_LABEL => QVariant::from(&QString::from(label)),
            ROLE_PROGRAM_LABEL_UNVERIFIED => QVariant::from(&false),
            _ => QVariant::default(),
        },
        VisibleEntry::HostHeader {
            process_key,
            domain_key,
            key,
            label,
            expanded,
            counts,
        } => match role {
            ROLE_ID => QVariant::from(&QString::from(&format!(
                "host:{process_key}|{domain_key}|{key}"
            ))),
            ROLE_DEPTH => QVariant::from(&2i32),
            ROLE_GROUP_PARENT_KEY => QVariant::from(&QString::from(domain_key)),
            // Everything else reads as a domain header with the host's data.
            _ => grouped_entry_data(
                &VisibleEntry::DomainHeader {
                    process_key: process_key.clone(),
                    key: key.clone(),
                    label: label.clone(),
                    expanded: *expanded,
                    counts: *counts,
                },
                role,
                store,
            ),
        },
        VisibleEntry::Row { id, host_key, .. } => {
            let Some(row) = store.row_by_id(id) else {
                return QVariant::default();
            };
            match role {
                ROLE_ID => QVariant::from(&QString::from(&row.id)),
                ROLE_PROCESS => QVariant::from(&QString::from(&row.process)),
                ROLE_HOST => QVariant::from(&QString::from(&row.dst_host)),
                ROLE_PORT => QVariant::from(&(row.dst_port as i32)),
                ROLE_PROTOCOL => QVariant::from(&QString::from(&row.protocol)),
                ROLE_VERDICT => {
                    let v = Verdict::of(row);
                    QVariant::from(&QString::from(v.as_token()))
                }
                ROLE_PENDING => QVariant::from(&is_pending(row)),
                ROLE_DEPTH => QVariant::from(&if host_key.is_some() { 3i32 } else { 2i32 }),
                ROLE_IS_GROUP_HEADER => QVariant::from(&false),
                ROLE_EXPANDED => QVariant::from(&false),
                ROLE_GROUP_KEY | ROLE_GROUP_PARENT_KEY | ROLE_GROUP_LABEL => {
                    QVariant::from(&QString::from(""))
                }
                ROLE_GROUP_TOTAL
                | ROLE_GROUP_PENDING
                | ROLE_GROUP_ALLOWED
                | ROLE_GROUP_DENIED
                | ROLE_GROUP_BLOCKLISTED => QVariant::from(&0i32),
                ROLE_GROUP_LAST_HOUR | ROLE_GROUP_TODAY => QVariant::from(&QString::from("[]")),
                ROLE_MATCHED_RULE => QVariant::from(&QString::from(matched_rule_name(row))),
                ROLE_MATCHED_RULE_DISPLAY => {
                    QVariant::from(&QString::from(&matched_rule_display(row)))
                }
                ROLE_ANSWERED_WHILE_PAUSED => QVariant::from(&answered_while_paused(row)),
                ROLE_OUTCOME_TEXT => QVariant::from(&QString::from(outcome_text(row))),
                ROLE_ANSWER_DEADLINE_MS => QVariant::from(&answer_deadline_ms(row)),
                ROLE_MAKE_RULE_OFFERED => QVariant::from(&crate::make_rule::offers_make_rule(row)),
                ROLE_PROCESS_PATH => QVariant::from(&QString::from(process_path_text(row))),
                ROLE_DIRECTION => QVariant::from(&QString::from(&row.direction)),
                ROLE_STARTED_AT_MS => QVariant::from(&started_at_ms(row)),
                ROLE_APP_ICON => QVariant::from(&QString::from(&app_icon_for_row(row))),
                ROLE_NEEDS_RULE => QVariant::from(&needs_rule_for_row(row)),
                ROLE_PROGRAM_LABEL
                | ROLE_PROGRAM_LABEL_UNVERIFIED
                | ROLE_PROGRAM_PATH_TAIL
                | ROLE_PROGRAM_PATH_FULL => program_label::role_for_row(row, role),
                _ => QVariant::default(),
            }
        }
    }
}
