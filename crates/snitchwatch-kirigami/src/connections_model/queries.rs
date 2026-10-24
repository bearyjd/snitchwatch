//! `ConnectionsModel`'s row queries and rule invokables.

use super::*;

impl qobject::ConnectionsModel {
    /// Parity 2: destination IP (plus byte counters, currently always 0 from
    /// the bridge and not displayed — issue #49) for a single row, for the
    /// pending-decision dialog's insight panel.
    pub(super) fn row_details_json(&self, id: &QString) -> QString {
        let id = id.to_string();
        let Some(row) = self.store.row_by_id(&id) else {
            return QString::from("{}");
        };

        #[derive(serde::Serialize)]
        struct RowDetails<'a> {
            #[serde(rename = "processPath")]
            process_path: &'a str,
            #[serde(rename = "dstHost")]
            dst_host: &'a str,
            #[serde(rename = "dstPort")]
            dst_port: u16,
            protocol: &'a str,
            #[serde(rename = "matchedRule")]
            matched_rule: &'a str,
            #[serde(rename = "dstIp")]
            dst_ip: &'a str,
            #[serde(rename = "bytesSent")]
            bytes_sent: u64,
            #[serde(rename = "bytesReceived")]
            bytes_received: u64,
            #[serde(rename = "bindableProcessPath")]
            bindable_process_path: bool,
            #[serde(rename = "alsoListedByDefault")]
            also_listed_by_default: bool,
        }

        let details = RowDetails {
            process_path: row.process_path.as_deref().unwrap_or_default(),
            dst_host: &row.dst_host,
            dst_port: row.dst_port,
            protocol: &row.protocol,
            matched_rule: row.matched_rule.as_deref().unwrap_or_default(),
            dst_ip: &row.dst_ip,
            bytes_sent: row.bytes_sent,
            bytes_received: row.bytes_received,
            bindable_process_path: is_bindable_process_path(
                row.process_path.as_deref().unwrap_or_default(),
            ),
            also_listed_by_default: crate::connections::outcome::may_be_listed_again(row),
        };
        QString::from(&serde_json::to_string(&details).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: row details serialize failed");
            "{}".to_string()
        }))
    }

    pub(super) fn level_rule_scope_text(
        &self,
        level: i32,
        program: &QString,
        key: &QString,
    ) -> QString {
        use crate::connections::level_rules::{resolve, Level, NodeRef};
        let Some(level) = Level::from_i32(level) else {
            return QString::from("Rule unavailable: unknown scope");
        };
        let program = program.to_string();
        let key = key.to_string();
        let target = resolve(NodeRef {
            level,
            program: &program,
            key: &key,
        });
        if !target.bindable {
            return QString::from("Rule unavailable: executable binding unknown");
        }
        if target.too_broad {
            return QString::from("Rule unavailable: this domain cannot be covered safely");
        }
        let destination = match target.effective {
            Level::Program => "any host".to_owned(),
            Level::Domain => format!("domain {key} and its subdomains"),
            Level::Host if key.parse::<std::net::IpAddr>().is_ok() => format!("exact IP {key}"),
            Level::Host => format!("exact host {key}"),
        };
        QString::from(&format!(
            "Executable {program}; {destination}; any port; any protocol"
        ))
    }

    pub(super) fn simulation_prefill_json(&self, id: &QString) -> QString {
        let Some(row) = self.store.row_by_id(&id.to_string()) else {
            return QString::from("{}");
        };
        let form = crate::rules::simulator::SimulationForm::for_connection(row);
        QString::from(&serde_json::to_string(&form).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: simulation prefill serialize failed");
            "{}".to_string()
        }))
    }

    pub(super) fn current_source_session(&self) -> QString {
        let session = self.source_session.filter(|id| {
            crate::bridge_runtime::handles().is_none_or(|handles| handles.is_current_session(*id))
        });
        QString::from(&session.map(|id| format!("{id}:")).unwrap_or_default())
    }

    pub(super) fn is_pending_row(&self, id: &QString) -> bool {
        self.store.is_pending(&id.to_string()).unwrap_or(false)
    }

    pub(super) fn row_id_at(&self, index: i32) -> QString {
        let Ok(index) = usize::try_from(index) else {
            return QString::default();
        };
        let id = if self.grouped {
            match self.grouped_projection.get(index) {
                Some(VisibleEntry::Row { id, .. }) => id.as_str(),
                _ => "",
            }
        } else {
            self.store.visible_row(index).map_or("", |r| r.id.as_str())
        };
        QString::from(id)
    }

    pub(super) fn oldest_pending_row_index(&self, except_ids_json: &QString) -> i32 {
        let except = parse_except_ids(&except_ids_json.to_string());
        let index = if self.grouped {
            // The projection's order is not the store's: rank ties by the
            // store position of each pending row.
            let store_order: HashMap<&str, usize> = self
                .store
                .rows()
                .iter()
                .enumerate()
                .filter(|(_, row)| is_pending(row))
                .map(|(i, row)| (row.id.as_str(), i))
                .collect();
            oldest_pending_position(
                self.grouped_projection.iter().map(|e| match e {
                    VisibleEntry::Row { id, .. } => self.store.row_by_id(id),
                    _ => None,
                }),
                &except,
                |id| store_order.get(id).copied().unwrap_or(usize::MAX),
            )
        } else {
            // The flat list draws rows in store order already.
            oldest_pending_position(
                (0..self.store.visible_len()).map(|i| self.store.visible_row(i)),
                &except,
                |_| 0,
            )
        };
        index.map_or(-1, |i| i as i32)
    }

    pub(super) fn visible_row_index(&self, id: &QString) -> i32 {
        let id = id.to_string();
        let index = if self.grouped {
            self.grouped_projection
                .iter()
                .position(|e| matches!(e, VisibleEntry::Row { id: row_id, .. } if *row_id == id))
        } else {
            self.store.visible_index_of(&id)
        };
        index.map_or(-1, |i| i as i32)
    }

    pub(super) fn make_rule(
        &self,
        id: &QString,
        choice: &QString,
        scope: &QString,
        duration: &QString,
        request_id: &QString,
    ) -> QString {
        let id = id.to_string();
        let made_at_ms = now_ms();
        let message = self.store.row_by_id(&id).and_then(|row| {
            let (choice, scope, duration, request_id) = (
                choice.to_string(),
                scope.to_string(),
                duration.to_string(),
                request_id.to_string(),
            );
            crate::make_rule::add_rule_message(
                row,
                &choice,
                &scope,
                &duration,
                made_at_ms,
                &request_id,
            )
        });
        let problem = crate::make_rule::send_problem(message, |msg| {
            crate::bridge_feed::dispatch_for_row(&id, msg)
        });
        QString::from(problem.unwrap_or(""))
    }

    pub(super) fn create_level_rule(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
        choice: &QString,
        confirmed: bool,
        shown_duration: &QString,
    ) -> QString {
        let session = self.source_session;
        level_rule_actions::create(
            self,
            level,
            process_key,
            key,
            choice,
            confirmed,
            shown_duration,
            session,
            false,
        )
    }

    pub(super) fn delete_level_rule(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
    ) -> QString {
        let session = self.source_session;
        level_rule_actions::delete(self, level, process_key, key, session, None)
    }

    pub(super) fn create_level_rule_for_session(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
        choice: &QString,
        shown_duration: &QString,
        source_session: &QString,
    ) -> QString {
        let session =
            match level_rule_actions::captured_session(self.source_session, source_session) {
                Ok(session) => session,
                Err(reason) => return QString::from(reason),
            };
        level_rule_actions::create(
            self,
            level,
            process_key,
            key,
            choice,
            // Only program confirmation sends a reviewed duration token.
            level == 0 && !shown_duration.is_empty(),
            shown_duration,
            Some(session),
            true,
        )
    }

    pub(super) fn delete_level_rule_for_session(
        self: Pin<&mut Self>,
        level: i32,
        process_key: &QString,
        key: &QString,
        source_session: &QString,
        rule_name: &QString,
        rule_state: &QString,
    ) -> QString {
        let session =
            match level_rule_actions::captured_session(self.source_session, source_session) {
                Ok(session) => session,
                Err(reason) => return QString::from(reason),
            };
        let (name, state) = (rule_name.to_string(), rule_state.to_string());
        level_rule_actions::delete(
            self,
            level,
            process_key,
            key,
            Some(session),
            Some((&name, &state)),
        )
    }

    pub(super) fn poll_level_rules(self: Pin<&mut Self>) {
        level_rule_actions::poll(self);
    }

    pub(super) fn level_rule_duration(&self) -> QString {
        QString::from(level_rule_actions::default_duration(self))
    }

    pub(super) fn refresh_level_rule_duration(self: Pin<&mut Self>) {
        if level_rule_actions::refresh_default_duration(&self) {
            self.notify_level_rules_changed();
        }
    }

    pub(super) fn inline_duration_for(
        &self,
        id: &QString,
        choice: &QString,
        app_bound_rules: bool,
        allow_duration: &QString,
    ) -> QString {
        let allow_duration = allow_duration.to_string();
        let token = VerdictChoice::from_token(&choice.to_string()).map_or("this_time", |choice| {
            inline_deny::duration_token_for_row(
                &self.store,
                &id.to_string(),
                choice,
                app_bound_rules,
                &allow_duration,
            )
        });
        QString::from(token)
    }

    pub(super) fn alert_row_json(&self) -> QString {
        QString::from(&crate::alert_row::oldest_pending_json(&self.store))
    }

    pub(super) fn inline_deny_for(&self, id: &QString, app_bound_rules: bool) -> QString {
        QString::from(InlineDeny::for_row(&self.store, &id.to_string(), app_bound_rules).token())
    }

    pub(super) fn first_pending_row_id_for_process(&self, key: &QString) -> QString {
        let ids = self.grouping.pending_row_ids(&key.to_string());
        QString::from(ids.first().map_or("", String::as_str))
    }

    /// Issue #18 batch actions: pending row ids under process group `key`,
    /// JSON-encoded as `["id1","id2",...]`. Returns `"[]"` for an unknown
    /// key or serialization failure rather than erroring — the QML caller
    /// degrades to a no-op loop.
    pub(super) fn pending_row_ids_for_process(mut self: Pin<&mut Self>, key: &QString) -> QString {
        self.as_mut().flush_grouped_messages();
        let ids = self.grouping.pending_row_ids(&key.to_string());
        QString::from(&serde_json::to_string(&ids).unwrap_or_else(|e| {
            tracing::error!(error = %e, "ConnectionsModel: pending row ids serialize failed");
            "[]".to_string()
        }))
    }

    pub(super) fn refresh_pending_age(mut self: Pin<&mut Self>) {
        let now = now_ms();
        let age = pending_age_secs(self.store.oldest_pending_started_at_ms(now), now);
        self.as_mut().set_oldest_pending_age_secs(age);
    }

    pub(super) fn pending_row_ids_for_group(
        mut self: Pin<&mut Self>,
        program: &QString,
        level: i32,
        key: &QString,
        parent_domain: &QString,
    ) -> QString {
        self.as_mut().flush_grouped_messages();
        let ids = self.grouping.pending_row_ids_for_group(
            &program.to_string(),
            level,
            &key.to_string(),
            &parent_domain.to_string(),
        );
        QString::from(&serde_json::to_string(&ids).expect("string ids serialize"))
    }
}
