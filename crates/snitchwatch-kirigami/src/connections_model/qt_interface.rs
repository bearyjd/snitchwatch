//! `ConnectionsModel`'s Qt model interface (rows, roles, filter setters, feed start).

use super::*;

impl qobject::ConnectionsModel {
    pub(super) fn row_count(&self, _parent: &QModelIndex) -> i32 {
        if self.grouped {
            self.grouped_projection.len() as i32
        } else {
            self.store.visible_len() as i32
        }
    }

    pub(super) unsafe fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        if role == ROLE_SOURCE_SESSION {
            return QVariant::from(&QString::from(
                &self
                    .source_session
                    .map(|id| format!("{id}:"))
                    .unwrap_or_default(),
            ));
        }
        if matches!(
            role,
            ROLE_LEVEL_RULE_STATE | ROLE_LEVEL_RULE_NAME | ROLE_LEVEL_RULE_REASON
        ) {
            return self.level_rule_role(index.row(), role);
        }
        if role == ROLE_RETRIED_COUNT {
            return self.retried_role(index.row());
        }
        if matches!(role, ROLE_REPEAT_COUNT | ROLE_LAST_SEEN_MS) {
            return self.repeat_role(index.row(), role);
        }
        if self.grouped {
            let Some(entry) = self.grouped_projection.get(index.row() as usize) else {
                return QVariant::default();
            };
            return grouped_entry_data(entry, role, &self.store);
        }
        let Some(row) = self.store.visible_row(index.row() as usize) else {
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
            ROLE_DEPTH => QVariant::from(&0i32),
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
            ROLE_MATCHED_RULE_DISPLAY => QVariant::from(&QString::from(&matched_rule_display(row))),
            ROLE_ANSWERED_WHILE_PAUSED => QVariant::from(&answered_while_paused(row)),
            ROLE_OUTCOME_TEXT => QVariant::from(&QString::from(outcome_text(row))),
            ROLE_ANSWER_DEADLINE_MS => QVariant::from(&answer_deadline_ms(row)),
            ROLE_MAKE_RULE_OFFERED => QVariant::from(&crate::make_rule::offers_make_rule(row)),
            ROLE_PROCESS_PATH => QVariant::from(&QString::from(process_path_text(row))),
            ROLE_DIRECTION => QVariant::from(&QString::from(&row.direction)),
            ROLE_STARTED_AT_MS => QVariant::from(&started_at_ms(row)),
            ROLE_APP_ICON => QVariant::from(&QString::from(&app_icon_for_row(row))),
            ROLE_NEEDS_RULE => QVariant::from(&needs_rule_for_row(row)),
            ROLE_GROUP_LEVEL => QVariant::from(&3i32),
            ROLE_GROUP_PROCESS_KEY => QVariant::from(&QString::from("")),
            ROLE_PROGRAM_LABEL
            | ROLE_PROGRAM_LABEL_UNVERIFIED
            | ROLE_PROGRAM_PATH_TAIL
            | ROLE_PROGRAM_PATH_FULL => program_label::role_for_row(row, role),
            _ => QVariant::default(),
        }
    }

    pub(super) fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::<QHashPair_i32_QByteArray>::default();
        roles.insert(ROLE_ID, QByteArray::from("rowId"));
        roles.insert(ROLE_SOURCE_SESSION, QByteArray::from("sourceSession"));
        roles.insert(ROLE_PROCESS, QByteArray::from("process"));
        roles.insert(ROLE_HOST, QByteArray::from("host"));
        roles.insert(ROLE_PORT, QByteArray::from("port"));
        roles.insert(ROLE_PROTOCOL, QByteArray::from("protocol"));
        roles.insert(ROLE_VERDICT, QByteArray::from("verdict"));
        roles.insert(ROLE_PENDING, QByteArray::from("pending"));
        roles.insert(ROLE_DEPTH, QByteArray::from("depth"));
        roles.insert(ROLE_IS_GROUP_HEADER, QByteArray::from("isGroupHeader"));
        roles.insert(ROLE_EXPANDED, QByteArray::from("expanded"));
        roles.insert(ROLE_GROUP_KEY, QByteArray::from("groupKey"));
        roles.insert(ROLE_GROUP_PARENT_KEY, QByteArray::from("groupParentKey"));
        roles.insert(ROLE_GROUP_LABEL, QByteArray::from("groupLabel"));
        roles.insert(ROLE_GROUP_TOTAL, QByteArray::from("groupTotal"));
        roles.insert(ROLE_GROUP_PENDING, QByteArray::from("groupPending"));
        roles.insert(ROLE_GROUP_ALLOWED, QByteArray::from("groupAllowed"));
        roles.insert(ROLE_GROUP_DENIED, QByteArray::from("groupDenied"));
        roles.insert(ROLE_GROUP_BLOCKLISTED, QByteArray::from("groupBlocklisted"));
        roles.insert(ROLE_GROUP_LAST_HOUR, QByteArray::from("groupLastHour"));
        roles.insert(ROLE_GROUP_TODAY, QByteArray::from("groupToday"));
        roles.insert(ROLE_MATCHED_RULE, QByteArray::from("matchedRule"));
        roles.insert(
            ROLE_MATCHED_RULE_DISPLAY,
            QByteArray::from("matchedRuleDisplay"),
        );
        roles.insert(
            ROLE_ANSWERED_WHILE_PAUSED,
            QByteArray::from("answeredWhilePaused"),
        );
        roles.insert(ROLE_OUTCOME_TEXT, QByteArray::from("outcomeText"));
        roles.insert(
            ROLE_ANSWER_DEADLINE_MS,
            QByteArray::from("answerDeadlineMs"),
        );
        roles.insert(ROLE_MAKE_RULE_OFFERED, QByteArray::from("makeRuleOffered"));
        roles.insert(ROLE_PROCESS_PATH, QByteArray::from("processPath"));
        roles.insert(ROLE_DIRECTION, QByteArray::from("direction"));
        roles.insert(ROLE_STARTED_AT_MS, QByteArray::from("startedAtMs"));
        roles.insert(ROLE_APP_ICON, QByteArray::from("appIcon"));
        roles.insert(ROLE_NEEDS_RULE, QByteArray::from("needsRule"));
        roles.insert(ROLE_GROUP_LEVEL, QByteArray::from("groupLevel"));
        roles.insert(ROLE_GROUP_PROCESS_KEY, QByteArray::from("groupProcessKey"));
        roles.insert(ROLE_LEVEL_RULE_STATE, QByteArray::from("levelRuleState"));
        roles.insert(ROLE_LEVEL_RULE_NAME, QByteArray::from("levelRuleName"));
        roles.insert(ROLE_LEVEL_RULE_REASON, QByteArray::from("levelRuleReason"));
        roles.insert(ROLE_REPEAT_COUNT, QByteArray::from("repeatCount"));
        roles.insert(ROLE_LAST_SEEN_MS, QByteArray::from("lastSeenMs"));
        roles.insert(ROLE_PROGRAM_LABEL, QByteArray::from("programLabel"));
        roles.insert(
            ROLE_PROGRAM_LABEL_UNVERIFIED,
            QByteArray::from("programLabelUnverified"),
        );
        roles.insert(ROLE_PROGRAM_PATH_TAIL, QByteArray::from("programPathTail"));
        roles.insert(ROLE_PROGRAM_PATH_FULL, QByteArray::from("programPathFull"));
        roles.insert(ROLE_RETRIED_COUNT, QByteArray::from("retriedCount"));
        roles
    }

    pub(super) fn load_demo_rows(self: Pin<&mut Self>) {
        if !crate::demo::enabled() {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.apply_server_message(ServerMessage::InsertConnectionRows {
            rows: crate::demo::demo_rows(now_ms),
        });
    }

    pub(super) fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(msg) => self.apply_server_message(msg),
            Err(e) => tracing::warn!(error = %e, "ConnectionsModel: bad ServerMessage JSON"),
        }
    }

    pub(super) fn set_filter_query(self: Pin<&mut Self>, query: &QString) {
        let mut filter = self.store.filter().clone();
        filter.set_query(query.to_string());
        self.apply_filter(filter);
    }

    pub(super) fn set_pending_only(self: Pin<&mut Self>, pending_only: bool) {
        let mut filter = self.store.filter().clone();
        filter.set_pending_only(pending_only);
        self.apply_filter(filter);
    }

    pub(super) fn set_denied_only(self: Pin<&mut Self>, denied_only: bool) {
        let mut filter = self.store.filter().clone();
        filter.set_denied_only(denied_only);
        self.apply_filter(filter);
    }

    pub(super) fn set_from_now(mut self: Pin<&mut Self>, from_now: bool) {
        let mut filter = self.store.filter().clone();
        let since = match (from_now, filter.since_ms()) {
            (true, Some(started)) => Some(started),
            (true, None) => Some(now_ms()),
            (false, _) => None,
        };
        let cut = since.is_some() && since != filter.since_ms();
        filter.set_since_ms(since);
        self.as_mut().apply_filter(filter);
        if cut {
            // A repeat after the cut must not join (and hide in) a row that
            // started before it.
            self.as_mut().rust_mut().collapse.seal();
        }
    }

    pub(super) fn set_current_row_id(mut self: Pin<&mut Self>, id: &QString) {
        self.as_mut().rust_mut().current_row_id = id.to_string();
    }

    pub(super) fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("ConnectionsModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let icons_thread = qt_thread.clone();
        crate::app_icons::when_ready(move || {
            let _ = icons_thread.queue(|qobject| qobject.refresh_app_icons());
        });
        let session_handles = handles.clone();
        let geo_preparer = std::sync::Mutex::new(crate::geo::prepared::GeoMessagePreparer::new(
            crate::geo::shared_resolver(),
        ));
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "ConnectionsModel",
            crate::bridge_dispatch::interests_monitor,
            move |connection_id, msg, _json| {
                let mut msg = msg.clone();
                qualify_connection_ids(&mut msg, connection_id);
                if !session_handles.is_current_session(connection_id) {
                    return;
                }
                // Retained facts cross the queue with the original all-row
                // message. No resolver call is permitted in the Qt callback.
                let prepared = geo_preparer
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .prepare(connection_id, &msg);
                let session_handles = session_handles.clone();
                let _ = qt_thread.queue(move |mut qobject| {
                    if !session_handles.is_current_session(connection_id) {
                        return;
                    }
                    qobject.as_mut().install_prepared_geo(prepared);
                    qobject.apply_message_for_session(msg, Some(connection_id));
                });
            },
        );
    }
}
