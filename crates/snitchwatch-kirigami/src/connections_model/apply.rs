//! `ConnectionsModel`'s message-application pipeline: buffering, grouped projection, row mutations.

use super::*;

impl qobject::ConnectionsModel {
    /// Apply a typed bridge message. In grouped mode with the bridge runtime
    /// available, messages are *buffered* and flushed in one Qt model reset
    /// per [`GROUPED_FLUSH_DEBOUNCE`] window ([`Self::flush_grouped_messages`])
    /// — a busy system's event bursts would otherwise cost one full reset
    /// (delegate churn, scroll/selection loss) per message. Everywhere else
    /// (flat mode, headless tests, bridge-less operation) the message applies
    /// immediately via [`Self::apply_now`].
    pub fn apply_server_message(mut self: Pin<&mut Self>, msg: ServerMessage) {
        self.as_mut().prepare_fixture_geo(&msg);
        self.as_mut().apply_message_for_session(msg, None);
    }

    /// The rule list and rule command results go straight to
    /// [`LevelRules`]: no buffering, no reset, only the two rule roles repaint.
    pub(super) fn apply_rules_message(mut self: Pin<&mut Self>, msg: &ServerMessage) {
        let applied = self.as_mut().rust_mut().level_rules.apply(msg);
        if applied.rules_changed || applied.finished.is_some() {
            self.as_mut().notify_level_rules_changed();
        }
        if let Some(finished) = applied.finished {
            self.report_level_finished(finished);
        }
    }

    /// Repaint the rule button of every header (a rule change can move any
    /// of them, and a header's state costs little to ask again).
    pub(super) fn notify_level_rules_changed(self: Pin<&mut Self>) {
        let len = self.grouped_projection.len();
        if !self.grouped || len == 0 {
            return;
        }
        let no_parent = QModelIndex::default();
        let (top, bottom) = (
            self.index(0, 0, &no_parent),
            self.index(len as i32 - 1, 0, &no_parent),
        );
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_LEVEL_RULE_STATE);
        roles.append(ROLE_LEVEL_RULE_NAME);
        roles.append(ROLE_LEVEL_RULE_REASON);
        self.data_changed(&top, &bottom, &roles);
    }

    pub(super) fn report_level_finished(mut self: Pin<&mut Self>, finished: LevelFinished) {
        self.as_mut().notify_level_rules_changed();
        self.level_rule_finished(
            finished.node.level as i32,
            QString::from(&finished.node.program),
            QString::from(&finished.node.key),
            finished.ending as i32,
            QString::from(&finished.finished.status),
        );
    }

    pub(super) fn level_rule_role(&self, row: i32, role: i32) -> QVariant {
        level_rule_actions::role(self, row, role)
    }

    /// The `repeatCount` / `lastSeenMs` roles of list row `row`.
    pub(super) fn repeat_role(&self, row: i32, role: i32) -> QVariant {
        let id = self.row_id_at(row).to_string();
        let found = self.store.row_by_id(&id);
        if role == ROLE_REPEAT_COUNT {
            let count = found.map_or(0, |_| self.collapse.repeat_count(&id));
            return QVariant::from(&(count as i32));
        }
        let last_seen = found.map_or(-1.0, |row| {
            self.collapse.last_seen_ms(&id).unwrap_or(row.started_at_ms) as f64
        });
        QVariant::from(&last_seen)
    }

    pub(super) fn apply_message_for_session(
        mut self: Pin<&mut Self>,
        msg: ServerMessage,
        connection_id: Option<u64>,
    ) {
        if matches!(msg, ServerMessage::RuleCommandResult { .. }) {
            self.apply_rules_message(&msg);
            return;
        }
        if matches!(
            msg,
            ServerMessage::SetRules { .. } | ServerMessage::UpdateRules { .. }
        ) {
            // Rows buffered in grouped mode land first, so rules apply in the
            // bridge's order: the snapshot's row replay before its rules,
            // which is what the first-seen baseline reads (0c).
            if !self.pending_grouped_msgs.is_empty() {
                self.as_mut().flush_grouped_messages();
            }
            self.as_mut().apply_rules_message(&msg);
            self.after_rules_applied();
            return;
        }
        if matches!(msg, ServerMessage::ClearConnectionRows) {
            // A new session's snapshot: no result can come for the old one.
            for finished in self.as_mut().rust_mut().level_rules.abandon_all() {
                self.as_mut().report_level_finished(finished);
            }
        }
        if self.grouped {
            if let Some(handles) = crate::bridge_runtime::handles() {
                let schedule = {
                    let mut rust = self.as_mut().rust_mut();
                    rust.pending_grouped_msgs.push(BufferedConnectionMessage {
                        connection_id,
                        message: msg,
                    });
                    if rust.grouped_flush_scheduled {
                        false
                    } else {
                        rust.grouped_flush_scheduled = true;
                        true
                    }
                };
                if schedule {
                    let qt_thread = self.qt_thread();
                    handles.runtime().spawn(async move {
                        tokio::time::sleep(GROUPED_FLUSH_DEBOUNCE).await;
                        let _ = qt_thread.queue(|qobject| qobject.flush_grouped_messages());
                    });
                }
                return;
            }
        }
        self.as_mut().rust_mut().source_session = connection_id;
        self.apply_now(msg);
    }

    /// Apply any grouped-mode messages still sitting in the debounce buffer,
    /// all inside a single Qt model reset, then re-run the auto-select policy
    /// over every pending row that arrived in the batch and re-anchor the
    /// user's selection (a reset clears the view's `currentIndex`).
    pub(super) fn flush_grouped_messages(mut self: Pin<&mut Self>) {
        let buffered = {
            let mut rust = self.as_mut().rust_mut();
            rust.grouped_flush_scheduled = false;
            std::mem::take(&mut rust.pending_grouped_msgs)
        };
        // Every flush path (timer, regroup, filter, batch action) reaches this
        // guard after the final Qt queue hop. Never revive an old session.
        let handles = crate::bridge_runtime::handles();
        // All retained authenticated messages belong to the current session.
        let source_session = buffered
            .iter()
            .filter_map(|entry| entry.connection_id)
            .find(|id| {
                handles
                    .as_ref()
                    .is_some_and(|handles| handles.is_current_session(*id))
            });
        let msgs = take_current_grouped_messages(buffered, |id| {
            handles
                .as_ref()
                .is_some_and(|handles| handles.is_current_session(id))
        });
        if msgs.is_empty() {
            return;
        }
        let new_pending_ids = collect_new_pending_ids(&msgs);
        let selected_before = self.current_row_id.clone();

        unsafe {
            self.as_mut().begin_reset_model();
        }
        {
            let mut rust = self.as_mut().rust_mut();
            rust.source_session = source_session;
            for msg in msgs {
                for msg in rust.collapse.rewrite(msg) {
                    mirror_into_grouping(&mut rust, &msg);
                    rust.store.apply(msg);
                }
            }
            if rust.store.is_filtered() {
                rust.store.recompute_visible();
            }
            let ConnectionsModelRust {
                store, collapse, ..
            } = &mut *rust;
            store.prune_prepared_geo(|id| collapse.contains_absorbed(id));
        }
        self.as_mut().rebuild_grouped_projection();
        unsafe {
            self.as_mut().end_reset_model();
        }
        self.as_mut().refresh_counts();
        let moved = self.as_mut().maybe_auto_select(&new_pending_ids);

        // If auto-select didn't move the selection, re-anchor the row the
        // user had selected before the reset (if it still exists/is visible).
        if !moved && !selected_before.is_empty() && self.current_row_id == selected_before {
            let vis = self
                .grouped_projection
                .iter()
                .position(|e| matches!(e, VisibleEntry::Row { id, .. } if id == &selected_before));
            if let Some(vis) = vis {
                self.as_mut().auto_select_requested(vis as i32);
            }
        }
    }

    /// The immediate (non-debounced) apply path: replay one message behind
    /// the correct Qt begin/end signals.
    pub(super) fn apply_now(mut self: Pin<&mut Self>, msg: ServerMessage) {
        // Ids that arrive *pending* in this batch — captured before mutation so
        // the auto-select policy can react to them afterward.
        let new_pending_ids = collect_new_pending_ids(std::slice::from_ref(&msg));
        // Repeated event rows collapse before the store and the tree see them.
        let msgs = self.as_mut().rust_mut().collapse.rewrite(msg);
        for msg in msgs {
            self.as_mut().apply_rewritten(msg);
        }
        {
            let mut rust = self.as_mut().rust_mut();
            let ConnectionsModelRust {
                store, collapse, ..
            } = &mut *rust;
            store.prune_prepared_geo(|id| collapse.contains_absorbed(id));
        }
        self.as_mut().refresh_counts();
        self.maybe_auto_select(&new_pending_ids);
    }

    /// Replay one (already collapsed) message behind the correct Qt
    /// begin/end signals.
    pub(super) fn apply_rewritten(mut self: Pin<&mut Self>, msg: ServerMessage) {
        // Mirror the same delta into the Process->Domain group tree, Qt-free
        // and incremental (see `connections::grouping` module docs), so both
        // projections stay live regardless of which one is currently on
        // screen. This must happen before `msg` is consumed by `store.apply`
        // below, hence matching on `&msg` here.
        {
            let mut rust = self.as_mut().rust_mut();
            mirror_into_grouping(&mut rust, &msg);
        }

        if self.grouped {
            // The grouped view's visible-row set (headers appearing/
            // disappearing, expand state interacting with membership changes)
            // doesn't map onto incremental begin/insert/remove brackets as
            // cleanly as the flat list does; mirroring the precedent the
            // filtered-flat branch below already sets ("a filter is an
            // explicit user investigation mode... a reset is acceptable
            // here"), the grouped view is refreshed via a full Qt reset while
            // the underlying tree/aggregates stay incrementally maintained.
            unsafe {
                self.as_mut().begin_reset_model();
            }
            {
                let mut rust = self.as_mut().rust_mut();
                rust.store.apply(msg);
                if rust.store.is_filtered() {
                    rust.store.recompute_visible();
                }
            }
            self.as_mut().rebuild_grouped_projection();
            unsafe {
                self.as_mut().end_reset_model();
            }
        } else if self.store.is_filtered() {
            // With an active filter the visible→store index mapping shifts in
            // ways that don't map cleanly onto incremental begin/insert/remove
            // brackets; a filter is an explicit user investigation mode, so a
            // reset is acceptable here (and doesn't affect the unfiltered
            // core-loop path below, which stays incremental to preserve scroll
            // position). Recompute the projection inside the reset bracket.
            unsafe {
                self.as_mut().begin_reset_model();
            }
            {
                let mut rust = self.as_mut().rust_mut();
                rust.store.apply(msg);
                rust.store.recompute_visible();
            }
            unsafe {
                self.as_mut().end_reset_model();
            }
        } else {
            match msg {
                ServerMessage::InsertConnectionRows { rows } => self.as_mut().apply_insert(rows),
                ServerMessage::RemoveConnectionRows { ids } => self.as_mut().apply_remove(&ids),
                ServerMessage::UpdateConnectionRows { rows } => self.as_mut().apply_update(rows),
                other => {
                    // Move/Clear (and no-op variants): bracket with a full reset
                    // — rare relative to insert/update, and always view-correct.
                    unsafe {
                        self.as_mut().begin_reset_model();
                    }
                    self.as_mut().rust_mut().store.apply(other);
                    unsafe {
                        self.as_mut().end_reset_model();
                    }
                }
            }
        }
    }

    /// Rebuild the cached flattened grouped projection from the current
    /// group tree, store rows, and active filter. Cheap relative to a tree
    /// rebuild — see `connections::grouping` module docs.
    pub(super) fn rebuild_grouped_projection(mut self: Pin<&mut Self>) {
        let projection = self.compute_grouped_projection();
        self.as_mut().rust_mut().grouped_projection = projection;
    }

    pub(super) fn compute_grouped_projection(&self) -> Vec<VisibleEntry> {
        let filter = self.store.filter().clone();
        let rows_by_id: HashMap<&str, &ConnectionRow> = self
            .store
            .rows()
            .iter()
            .map(|r| (r.id.as_str(), r))
            .collect();
        self.grouping.build_host_projection(&rows_by_id, &filter)
    }

    /// Swap the grouped projection for `new` with ranged signals (one remove
    /// range, one insert range, `dataChanged` for headers whose expand flag or
    /// counts changed) instead of a model reset, so the view keeps its scroll
    /// position, selection and delegates. See `connections::projection_diff`.
    pub(super) fn apply_grouped_projection(mut self: Pin<&mut Self>, new: Vec<VisibleEntry>) {
        let diff = projection_diff::diff(&self.grouped_projection, &new);
        let no_parent = QModelIndex::default();
        if !diff.removed.is_empty() {
            unsafe {
                self.as_mut().begin_remove_rows(
                    &no_parent,
                    diff.removed.start as i32,
                    (diff.removed.end - 1) as i32,
                );
            }
            self.as_mut()
                .rust_mut()
                .grouped_projection
                .drain(diff.removed.clone());
            unsafe {
                self.as_mut().end_remove_rows();
            }
        }
        if !diff.inserted.is_empty() {
            unsafe {
                self.as_mut().begin_insert_rows(
                    &no_parent,
                    diff.inserted.start as i32,
                    (diff.inserted.end - 1) as i32,
                );
            }
            self.as_mut().rust_mut().grouped_projection.splice(
                diff.inserted.start..diff.inserted.start,
                new[diff.inserted.clone()].iter().cloned(),
            );
            unsafe {
                self.as_mut().end_insert_rows();
            }
        }
        for &idx in &diff.changed {
            self.as_mut().rust_mut().grouped_projection[idx] = new[idx].clone();
            let index = self.index(idx as i32, 0, &no_parent);
            self.as_mut()
                .data_changed(&index, &index, &QList::<i32>::default());
        }
        debug_assert_eq!(self.grouped_projection, new, "ranged update desynced");
    }

    /// Replace the active filter, bracketed by a model reset (the whole visible
    /// set changes), and refresh the derived counts. Filter/search applies in
    /// both flat and grouped modes — the grouped projection is rebuilt too so
    /// matched children keep their ancestor group headers visible.
    pub(super) fn apply_filter(mut self: Pin<&mut Self>, filter: ConnectionFilter) {
        // Anything still in the grouped debounce buffer must land first so the
        // filter is computed over current data.
        self.as_mut().flush_grouped_messages();
        if self.grouped {
            self.as_mut().rust_mut().store.set_filter(filter);
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
        } else {
            unsafe {
                self.as_mut().begin_reset_model();
            }
            self.as_mut().rust_mut().store.set_filter(filter);
            unsafe {
                self.as_mut().end_reset_model();
            }
        }
        self.as_mut().refresh_filter_state();
        self.refresh_counts();
    }

    /// Mirror the store's filter into the `filter*` properties.
    pub(super) fn refresh_filter_state(mut self: Pin<&mut Self>) {
        let filter = self.store.filter();
        let text = QString::from(filter.query());
        let kind = QString::from(filter.geo_scope().map(GeoScope::kind).unwrap_or(""));
        let key = QString::from(&filter.geo_scope().map(GeoScope::key).unwrap_or_default());
        let active = filter.is_active();
        let summary = QString::from(&filter.summary());
        let since = filter.since_ms().unwrap_or(0);
        let (pending_only, denied_only) = (filter.pending_only(), filter.denied_only());
        self.as_mut().set_geo_scope_kind(kind);
        self.as_mut().set_geo_scope_key(key);
        self.as_mut().set_filter_text(text);
        self.as_mut().set_filter_active(active);
        self.as_mut().set_filter_summary(summary);
        self.as_mut().set_filter_since_ms(since);
        self.as_mut().set_filter_pending_only(pending_only);
        self.as_mut().set_filter_denied_only(denied_only);
        let needs_attention = self.store.filter().needs_attention();
        self.as_mut().set_filter_needs_attention(needs_attention);
    }

    /// Apply the auto-select-on-new-pending-row policy. Sets `autoSelectRowId`
    /// (which QML watches) when a freshly arrived pending row should take the
    /// selection, without stealing focus from a pending row the user is on.
    /// Returns whether it asked QML to select a row. It does not adopt the row
    /// as the current one: QML may decline (a panel is showing), and says so by
    /// not calling `setCurrentRowId`.
    pub(super) fn maybe_auto_select(mut self: Pin<&mut Self>, new_pending_ids: &[String]) -> bool {
        if new_pending_ids.is_empty() {
            return false;
        }
        let current = self.current_row_id.clone();
        let current_opt = if current.is_empty() {
            None
        } else {
            Some(current.as_str())
        };
        let current_is_pending = current_opt
            .and_then(|id| self.store.is_pending(id))
            .unwrap_or(false);
        let refs: Vec<&str> = new_pending_ids.iter().map(String::as_str).collect();
        let Some(target) =
            crate::connections::filter::auto_select_target(current_opt, current_is_pending, &refs)
        else {
            return false;
        };
        // Translate the chosen id to its current *visible* index for QML. If the
        // target is filtered out of view there's nothing to select. In grouped
        // mode this is a position in the flattened projection rather than the
        // flat store's visible list. A new pending row's host starts collapsed,
        // so when its program and domain are open (issue #20: never force open
        // what the user collapsed) open just the host, with ranged signals, so
        // the row is in the projection to select.
        if self.grouped
            && self
                .as_mut()
                .rust_mut()
                .grouping
                .open_host_of_row_under_open_domain(&target)
        {
            let projection = self.compute_grouped_projection();
            self.as_mut().apply_grouped_projection(projection);
            self.as_mut().refresh_counts();
        }
        let vis = if self.grouped {
            self.grouped_projection
                .iter()
                .position(|entry| matches!(entry, VisibleEntry::Row { id, .. } if id == &target))
        } else {
            self.store.visible_index_of(&target)
        };
        let Some(vis) = vis else {
            return false;
        };
        self.as_mut().auto_select_requested(vis as i32);
        true
    }

    pub(super) fn apply_insert(mut self: Pin<&mut Self>, rows: Vec<ConnectionRow>) {
        // beginInsertRows must bracket exactly the range the store will grow by.
        // Derive that count from the store's own dedup-aware `count_new_ids`,
        // which mirrors `insert_rows` including the duplicate-id-in-one-batch
        // case (a naive filter against the pre-insert store over-counts there
        // and would desync the bracket from the real store delta — a
        // QAbstractListModel invariant violation).
        let existing = self.store.len();
        let appended = self.store.count_new_ids(&rows);

        if appended > 0 {
            let first = existing as i32;
            let last = (existing + appended - 1) as i32;
            unsafe {
                self.as_mut()
                    .begin_insert_rows(&QModelIndex::default(), first, last);
            }
        }
        let ops = self.as_mut().rust_mut().store.insert_rows(rows);
        // The bracketed count must equal the store's actual growth. This is the
        // snapshot-before/after reconciliation guarding the Qt invariant.
        debug_assert_eq!(
            self.store.len(),
            existing + appended,
            "beginInsertRows bracket ({appended}) desynced from actual store delta"
        );
        if appended > 0 {
            unsafe {
                self.as_mut().end_insert_rows();
            }
        }
        self.emit_changed_ops(&ops);
    }

    pub(super) fn apply_remove(mut self: Pin<&mut Self>, ids: &[String]) {
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.store.rows().iter().position(|r| &r.id == id))
            .collect();
        if indices.is_empty() {
            return;
        }
        indices.sort_unstable();
        indices.dedup();
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for &i in &indices {
            match runs.last_mut() {
                Some(run) if run.1 == i => run.1 = i + 1,
                _ => runs.push((i, i + 1)),
            }
        }
        // High-index run first so lower indices stay valid as we remove.
        for &(start, end) in runs.iter().rev() {
            unsafe {
                self.as_mut().begin_remove_rows(
                    &QModelIndex::default(),
                    start as i32,
                    (end - 1) as i32,
                );
            }
            self.as_mut().rust_mut().store.remove_range(start, end);
            unsafe {
                self.as_mut().end_remove_rows();
            }
        }
    }

    pub(super) fn apply_update(mut self: Pin<&mut Self>, rows: Vec<ConnectionRow>) {
        let ops = self.as_mut().rust_mut().store.update_rows(rows);
        self.emit_changed_ops(&ops);
    }

    /// The icon index just became ready: rows drawn before then have the
    /// generic icon and no `.desktop` name, so every row's `appIcon` and
    /// program-label roles are read again.
    pub(super) fn refresh_app_icons(mut self: Pin<&mut Self>) {
        let rows = self.row_count(&QModelIndex::default());
        if rows <= 0 {
            return;
        }
        let tl = self.index(0, 0, &QModelIndex::default());
        let br = self.index(rows - 1, 0, &QModelIndex::default());
        let mut roles = QList::<i32>::default();
        roles.append(ROLE_APP_ICON);
        roles.append(ROLE_PROGRAM_LABEL);
        roles.append(ROLE_PROGRAM_LABEL_UNVERIFIED);
        roles.append(ROLE_PROGRAM_PATH_TAIL);
        roles.append(ROLE_PROGRAM_PATH_FULL);
        self.as_mut().data_changed(&tl, &br, &roles);
    }

    pub(super) fn emit_changed_ops(mut self: Pin<&mut Self>, ops: &[ModelOp]) {
        for op in ops {
            if let ModelOp::Changed { indices } = op {
                for &idx in indices {
                    let tl = self.index(idx as i32, 0, &QModelIndex::default());
                    let br = self.index(idx as i32, 0, &QModelIndex::default());
                    // Empty roles list = "all roles changed" — the view re-reads.
                    let roles = QList::<i32>::default();
                    self.as_mut().data_changed(&tl, &br, &roles);
                }
            }
        }
    }

    pub(super) fn refresh_counts(mut self: Pin<&mut Self>) {
        // `count` tracks what the ListView actually shows: the grouped
        // projection's row count (headers + visible leaves) in grouped mode,
        // the flat filtered/unfiltered visible length otherwise.
        let visible = if self.grouped {
            self.grouped_projection.len() as i32
        } else {
            self.store.visible_len() as i32
        };
        let matching = self.store.visible_len() as i32;
        let total = self.store.len() as i32;
        let pending = self.store.rows().iter().filter(|r| is_pending(r)).count() as i32;
        // `totalCount` stays the whole store so the view can distinguish
        // empty from filtered.
        self.as_mut().set_count(visible);
        self.as_mut().set_matching_count(matching);
        self.as_mut().set_total_count(total);
        self.as_mut().set_pending_count(pending);
        self.as_mut().refresh_attention_counts();
        self.as_mut().refresh_pending_age();
    }
}
