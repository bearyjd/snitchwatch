//! `ConnectionsModel`'s exception-first default (clarity 0c; plan
//! `docs/superpowers/plans/2026-10-27-exception-first-default.md`): the
//! `needs_attention` switch, the first-seen baseline and acknowledgement,
//! and the "Background and system" counts. Which rows are handled is
//! `connections::attention`; this only keeps its facts current and forces
//! the projection to follow them.

use std::sync::Arc;

use super::*;
use crate::connections::attention::AttentionFacts;
use crate::connections::row_store::{RowStep, VisibleChange};
use crate::connections::seen_programs::{SeenPrograms, SEEN_FILE_NAME};

impl qobject::ConnectionsModel {
    pub(super) fn set_needs_attention(mut self: Pin<&mut Self>, on: bool) {
        let mut filter = self.store.filter().clone();
        filter.set_needs_attention(on);
        self.as_mut().apply_filter(filter);
        self.as_mut().rebuild_handled_counts();
        self.refresh_attention_counts();
    }

    pub(super) fn mark_new_programs_seen(mut self: Pin<&mut Self>) {
        if self.as_mut().rust_mut().seen.acknowledge(now_ms()) {
            self.refresh_attention_facts();
        }
    }

    pub(super) fn dismiss_baseline_notice(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().seen.dismiss_notice();
        self.refresh_attention_counts();
    }

    pub(super) fn buffer_server_message_json_for_tests(mut self: Pin<&mut Self>, json: &QString) {
        match serde_json::from_str::<ServerMessage>(&json.to_string()) {
            Ok(message) => {
                self.as_mut()
                    .rust_mut()
                    .pending_grouped_msgs
                    .push(BufferedConnectionMessage {
                        connection_id: None,
                        message,
                    })
            }
            Err(e) => tracing::warn!(error = %e, "ConnectionsModel: bad ServerMessage JSON"),
        }
    }

    /// The live GUI keeps its seen set in the config directory; headless
    /// tests and demo mode never touch it (`SeenPrograms::default`).
    pub(super) fn use_persisted_seen_programs(mut self: Pin<&mut Self>) {
        if crate::demo::enabled() {
            return;
        }
        let path = crate::paths::config_dir().join(SEEN_FILE_NAME);
        self.as_mut().rust_mut().seen =
            SeenPrograms::persisted(path, crate::logging::effective_uid());
    }

    /// A `SetRules`/`UpdateRules` was applied: take the baseline if this is
    /// the first one after a clear, then re-decide every row. Only a
    /// `SetRules` (`snapshot`) restores collapsing after a clear.
    pub(super) fn after_rules_applied(mut self: Pin<&mut Self>, snapshot: bool) {
        {
            let mut rust = self.as_mut().rust_mut();
            rust.attention_armed |= snapshot;
            let ConnectionsModelRust { store, seen, .. } = &mut *rust;
            let present = store
                .rows()
                .iter()
                .filter_map(|row| row.process_path.as_deref());
            seen.on_rules_applied(present, now_ms());
        }
        self.refresh_attention_facts();
    }

    /// Rebuild the "handled" facts from the CURRENT rules list and seen set
    /// (no rules until this session's snapshot arrived); only when they
    /// changed, recompute the visible rows, so a rule deleted or disabled
    /// later shows its rows again.
    pub(super) fn refresh_attention_facts(mut self: Pin<&mut Self>) {
        let rules = if self.attention_armed {
            self.level_rules.rules()
        } else {
            &[]
        };
        let facts = AttentionFacts::new(rules, self.seen.seen_paths());
        if self.store.filter().attention_facts() == Some(&facts) {
            self.refresh_attention_counts();
            return;
        }
        let facts = Arc::new(facts);
        if self.grouped {
            self.as_mut().rust_mut().store.set_attention_facts(facts);
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
        } else {
            unsafe {
                self.as_mut().begin_reset_model();
            }
            self.as_mut().rust_mut().store.set_attention_facts(facts);
            unsafe {
                self.as_mut().end_reset_model();
            }
        }
        self.as_mut().rebuild_handled_counts();
        self.as_mut().refresh_filter_state();
        self.refresh_counts();
    }

    /// Recount the group from every row: only when the facts or the switch
    /// change. With the full list showing nothing is counted.
    fn rebuild_handled_counts(mut self: Pin<&mut Self>) {
        let mut rust = self.as_mut().rust_mut();
        let ConnectionsModelRust {
            store,
            handled_counts,
            ..
        } = &mut *rust;
        let filter = store.filter();
        handled_counts.rebuild(
            filter.needs_attention(),
            filter.attention_facts(),
            store.rows(),
        );
    }

    /// The group's one-line summary figures (kept per row), the new-program
    /// count, the baseline notice and the filter state a clear may change.
    pub(super) fn refresh_attention_counts(mut self: Pin<&mut Self>) {
        let count = self.handled_counts.rows() as i32;
        let programs = self.handled_counts.programs() as i32;
        let available = self
            .store
            .filter()
            .attention_facts()
            .is_some_and(AttentionFacts::collapses_anything);
        let new_programs = self.seen.new_count() as i32;
        let notice = QString::from(self.seen.notice());
        self.as_mut().set_background_count(count);
        self.as_mut().set_background_programs(programs);
        self.as_mut().set_background_available(available);
        self.as_mut().set_new_program_count(new_programs);
        self.as_mut().set_baseline_notice(notice);
        self.refresh_filter_state();
    }

    /// The flat view while only the collapse narrows it: each changed row is
    /// re-decided alone, behind ranged signals, never a reset.
    pub(super) fn apply_collapsed_delta(mut self: Pin<&mut Self>, msg: ServerMessage) {
        match msg {
            ServerMessage::InsertConnectionRows { rows } => {
                for row in rows {
                    let step = self.store.plan_upsert(&row);
                    self.as_mut()
                        .signal_step(step, |store| store.commit_upsert(step, row));
                }
            }
            ServerMessage::UpdateConnectionRows { rows } => {
                for row in rows {
                    let step = self.store.plan_upsert(&row);
                    if step.index.is_some() {
                        self.as_mut()
                            .signal_step(step, |store| store.commit_upsert(step, row));
                    }
                }
            }
            ServerMessage::RemoveConnectionRows { ids } => {
                for id in ids {
                    if let Some(step) = self.store.plan_remove(&id) {
                        self.as_mut()
                            .signal_step(step, |store| store.commit_remove(step));
                    }
                }
            }
            other => self.as_mut().rust_mut().store.apply_incremental(other),
        }
    }

    fn signal_step(mut self: Pin<&mut Self>, step: RowStep, commit: impl FnOnce(&mut RowStore)) {
        let no_parent = QModelIndex::default();
        match step.change {
            VisibleChange::Inserted(at) => unsafe {
                self.as_mut()
                    .begin_insert_rows(&no_parent, at as i32, at as i32);
                commit(&mut self.as_mut().rust_mut().store);
                self.as_mut().end_insert_rows();
            },
            VisibleChange::Removed(at) => unsafe {
                self.as_mut()
                    .begin_remove_rows(&no_parent, at as i32, at as i32);
                commit(&mut self.as_mut().rust_mut().store);
                self.as_mut().end_remove_rows();
            },
            VisibleChange::Changed(at) => {
                commit(&mut self.as_mut().rust_mut().store);
                let index = self.index(at as i32, 0, &no_parent);
                self.as_mut()
                    .data_changed(&index, &index, &QList::<i32>::default());
            }
            VisibleChange::None => commit(&mut self.as_mut().rust_mut().store),
        }
    }
}
