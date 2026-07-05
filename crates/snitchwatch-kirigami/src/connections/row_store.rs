//! Pure, Qt-free row store for the connections list (Task 6 core).
//!
//! This holds the ordered list of [`ConnectionRow`]s and translates the
//! bridge's typed [`ServerMessage`] connection events into a sequence of
//! [`ModelOp`]s describing the precise index ranges that changed. The cxx-qt
//! `ConnectionsModel` wrapper (see `model.rs`) replays those ops around the
//! matching `beginInsertRows`/`endInsertRows` / `beginRemoveRows` etc. calls so
//! the QML `ListView` stays in sync.
//!
//! Keeping this logic Qt-free is what makes Task 6's acceptance test —
//! "seed the model with synthetic cache events, assert row count/order/content"
//! — a plain `#[test]` with no Qt runtime, mirroring the bridge's own cache
//! unit tests in intent.
//!
//! This is the direct re-homing of `web/js/connections.js`'s
//! `insertConnectionRows` / `updateConnectionRows` / `removeConnectionRows` /
//! `moveConnetionRows` handlers (note the upstream `moveConnetion` typo, which
//! the WS protocol preserves) into Rust.

use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};

use super::filter::ConnectionFilter;

/// A single QAbstractListModel mutation, in the exact shape Qt's
/// begin/end row-mutation protocol needs. Indices are into the *current*
/// visible order at the moment the op is applied, in sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOp {
    /// `count` rows were inserted starting at `at`.
    Insert { at: usize, count: usize },
    /// `count` rows were removed starting at `at`.
    Remove { at: usize, count: usize },
    /// Existing rows at these indices had their data changed in place.
    Changed { indices: Vec<usize> },
    /// The whole model was rebuilt; the view must reset.
    Reset,
}

/// Verdict/display state of a row, derived from `ConnectionRow.action`.
///
/// `action == None` is a pending prompt; `Some("allow"|"deny"|"blocklist")`
/// are decided states. This mirrors the `◐` pending marker / allow-green /
/// deny-red styling from the design spec's pending-row inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pending,
    Allowed,
    Denied,
    Blocklisted,
    /// Decided, but with an action string we don't recognise — shown neutrally.
    Other,
}

impl Verdict {
    pub fn from_action(action: Option<&str>) -> Self {
        match action {
            None => Verdict::Pending,
            Some("allow") => Verdict::Allowed,
            Some("deny") => Verdict::Denied,
            Some("blocklist") => Verdict::Blocklisted,
            Some(_) => Verdict::Other,
        }
    }

    /// Stable lowercase token for QML styling / role exposure.
    pub fn as_token(&self) -> &'static str {
        match self {
            Verdict::Pending => "pending",
            Verdict::Allowed => "allowed",
            Verdict::Denied => "denied",
            Verdict::Blocklisted => "blocklisted",
            Verdict::Other => "other",
        }
    }
}

/// Ordered, id-addressable store of connection rows.
///
/// The store always holds the *full* row list (`rows`). When `filter` is
/// active it additionally maintains `visible`, an ordered list of indices into
/// `rows` for the rows that pass the filter; the cxx-qt wrapper reads the
/// visible projection so the QML `ListView` shows only matching rows. When the
/// filter is inactive `visible` is left empty and the visible list is treated
/// as identity over `rows`, keeping the unfiltered core-loop path allocation-
/// free and incrementally updatable.
#[derive(Debug, Default)]
pub struct RowStore {
    rows: Vec<ConnectionRow>,
    filter: ConnectionFilter,
    visible: Vec<usize>,
}

impl RowStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[ConnectionRow] {
        &self.rows
    }

    pub fn row(&self, index: usize) -> Option<&ConnectionRow> {
        self.rows.get(index)
    }

    /// Look up a row by id regardless of filter state. Used by the grouping
    /// projection (`connections::grouping`) to resolve the `ConnectionRow`
    /// content for ids the group tree tracks by id only.
    pub fn row_by_id(&self, id: &str) -> Option<&ConnectionRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Number of rows in `incoming` that [`insert_rows`](Self::insert_rows)
    /// will actually append as *new* rows: ids not already present in the store
    /// **and** not already seen earlier within this same batch.
    ///
    /// The cxx-qt `ConnectionsModel` wrapper brackets its `beginInsertRows`
    /// call with this count *before* mutating the store, so it must match the
    /// store's real growth exactly. A naive "filter incoming against the
    /// current store" prediction over-counts when one batch carries the same
    /// new id twice (the second occurrence is folded into an in-place update by
    /// `insert_rows`, not a second append) — which would bracket more rows than
    /// were inserted and violate a `QAbstractListModel` invariant. This helper
    /// mirrors `insert_rows`'s dedup-against-store-*and*-within-batch behaviour
    /// so the two never disagree.
    pub fn count_new_ids(&self, incoming: &[ConnectionRow]) -> usize {
        let mut seen: std::collections::HashSet<&str> =
            self.rows.iter().map(|r| r.id.as_str()).collect();
        incoming
            .iter()
            .filter(|r| seen.insert(r.id.as_str()))
            .count()
    }

    pub fn verdict_at(&self, index: usize) -> Option<Verdict> {
        self.rows
            .get(index)
            .map(|r| Verdict::from_action(r.action.as_deref()))
    }

    fn position_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    /// Apply one bridge `ServerMessage`. Only the connection-list variants
    /// mutate the store; every other variant is a no-op here (they belong to
    /// other models — rules, blocklists, traffic — handled elsewhere).
    pub fn apply(&mut self, msg: ServerMessage) -> Vec<ModelOp> {
        match msg {
            ServerMessage::InsertConnectionRows { rows } => self.insert_rows(rows),
            ServerMessage::UpdateConnectionRows { rows } => self.update_rows(rows),
            ServerMessage::RemoveConnectionRows { ids } => self.remove_rows(&ids),
            ServerMessage::MoveConnetionRows { ids } => self.move_rows_to_front(&ids),
            ServerMessage::ClearConnectionRows => self.clear(),
            _ => Vec::new(),
        }
    }

    /// Insert appends new rows at the end. If an incoming row's id already
    /// exists it is treated as an update-in-place instead of a duplicate
    /// insert (the bridge can re-`insert` a row it already sent).
    pub fn insert_rows(&mut self, incoming: Vec<ConnectionRow>) -> Vec<ModelOp> {
        let mut ops = Vec::new();
        let mut changed = Vec::new();
        let mut appended = 0usize;
        let mut append_start = self.rows.len();

        for row in incoming {
            if let Some(idx) = self.position_of(&row.id) {
                self.rows[idx] = row;
                changed.push(idx);
            } else {
                if appended == 0 {
                    append_start = self.rows.len();
                }
                self.rows.push(row);
                appended += 1;
            }
        }

        if !changed.is_empty() {
            ops.push(ModelOp::Changed { indices: changed });
        }
        if appended > 0 {
            ops.push(ModelOp::Insert {
                at: append_start,
                count: appended,
            });
        }
        ops
    }

    /// Update replaces rows in place, matched by id. Unknown ids are ignored
    /// (an update for a row we've already evicted is not an error).
    pub fn update_rows(&mut self, incoming: Vec<ConnectionRow>) -> Vec<ModelOp> {
        let mut changed = Vec::new();
        for row in incoming {
            if let Some(idx) = self.position_of(&row.id) {
                self.rows[idx] = row;
                changed.push(idx);
            }
        }
        if changed.is_empty() {
            Vec::new()
        } else {
            vec![ModelOp::Changed { indices: changed }]
        }
    }

    /// Remove rows by id. Emits one `Remove` op per contiguous run so the
    /// view's `beginRemoveRows` ranges stay valid; ids are removed
    /// highest-index-first internally so earlier removals don't shift the
    /// indices of later ones.
    pub fn remove_rows(&mut self, ids: &[String]) -> Vec<ModelOp> {
        let mut indices: Vec<usize> = ids.iter().filter_map(|id| self.position_of(id)).collect();
        if indices.is_empty() {
            return Vec::new();
        }
        indices.sort_unstable();
        indices.dedup();

        // Coalesce into contiguous [start, end) runs, then remove/emit from the
        // highest run down so lower indices remain valid as we go.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for &i in &indices {
            match runs.last_mut() {
                Some(run) if run.1 == i => run.1 = i + 1,
                _ => runs.push((i, i + 1)),
            }
        }

        let mut ops = Vec::new();
        for &(start, end) in runs.iter().rev() {
            self.rows.drain(start..end);
            ops.push(ModelOp::Remove {
                at: start,
                count: end - start,
            });
        }
        ops
    }

    /// Move the identified rows to the front, preserving their relative order.
    ///
    /// Semantics note: `web/js/connections.js`'s `moveConnetionRows` reorders
    /// rows that gained fresh activity toward the top of the list. The exact
    /// upstream ordering is documented in the design spec's WS message mapping
    /// table; this implements move-to-front (most-recent-first), which is the
    /// observable behaviour, and models it to the view as remove-then-insert so
    /// the `ListView` reconciles correctly without `beginMoveRows` gymnastics.
    pub fn move_rows_to_front(&mut self, ids: &[String]) -> Vec<ModelOp> {
        // Collect target rows in the order the ids were given, skipping unknowns.
        let mut positions: Vec<usize> = Vec::new();
        for id in ids {
            if let Some(p) = self.position_of(id) {
                if !positions.contains(&p) {
                    positions.push(p);
                }
            }
        }
        if positions.is_empty() {
            return Vec::new();
        }
        // If they're already exactly the front block in order, nothing to do.
        let already_front = positions.iter().enumerate().all(|(k, &p)| k == p);
        if already_front {
            return Vec::new();
        }

        // Extract in id order, then reinsert at the front.
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        let mut extracted: Vec<ConnectionRow> = Vec::with_capacity(positions.len());
        // Map id-order -> row by removing from highest index down to keep
        // indices valid, but preserve the requested id order in `extracted`.
        let by_id_order = positions.clone();
        // Remove highest-first.
        let mut removal = sorted.clone();
        removal.sort_unstable_by(|a, b| b.cmp(a));
        // Stash rows keyed by original index.
        let mut stash: std::collections::HashMap<usize, ConnectionRow> =
            std::collections::HashMap::new();
        for &idx in &removal {
            stash.insert(idx, self.rows.remove(idx));
        }
        for idx in by_id_order {
            if let Some(r) = stash.remove(&idx) {
                extracted.push(r);
            }
        }

        let ops_removed = coalesce_removals(&sorted);
        let count = extracted.len();
        // Reinsert at the front in id order.
        for (k, row) in extracted.into_iter().enumerate() {
            self.rows.insert(k, row);
        }

        let mut ops = ops_removed;
        ops.push(ModelOp::Insert { at: 0, count });
        ops
    }

    /// Remove a half-open `[start, end)` index range directly. Used by the
    /// cxx-qt model wrapper, which computes the exact runs itself so it can
    /// bracket each with `beginRemoveRows`/`endRemoveRows`. Out-of-range
    /// bounds are clamped.
    pub fn remove_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.rows.len());
        if start < end {
            self.rows.drain(start..end);
        }
    }

    pub fn clear(&mut self) -> Vec<ModelOp> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        self.rows.clear();
        self.visible.clear();
        vec![ModelOp::Reset]
    }

    // --- Filtering / visible projection (Task 8) -----------------------------

    /// Is a narrowing filter currently applied?
    pub fn is_filtered(&self) -> bool {
        self.filter.is_active()
    }

    pub fn filter(&self) -> &ConnectionFilter {
        &self.filter
    }

    /// Replace the filter and recompute the visible projection. Returns whether
    /// the filter's active-ness or contents actually changed (callers use this
    /// to decide if a model reset is warranted).
    pub fn set_filter(&mut self, filter: ConnectionFilter) -> bool {
        if filter == self.filter {
            return false;
        }
        self.filter = filter;
        self.recompute_visible();
        true
    }

    /// Recompute `visible` from the current filter. No-op-ish when inactive
    /// (clears `visible`; the visible list is then identity over `rows`).
    pub fn recompute_visible(&mut self) {
        if !self.filter.is_active() {
            self.visible.clear();
            return;
        }
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| self.filter.matches(r))
            .map(|(i, _)| i)
            .collect();
    }

    /// Number of rows currently visible (post-filter).
    pub fn visible_len(&self) -> usize {
        if self.filter.is_active() {
            self.visible.len()
        } else {
            self.rows.len()
        }
    }

    /// The row at visible position `vis_index` (post-filter ordering).
    pub fn visible_row(&self, vis_index: usize) -> Option<&ConnectionRow> {
        if self.filter.is_active() {
            self.visible.get(vis_index).and_then(|&i| self.rows.get(i))
        } else {
            self.rows.get(vis_index)
        }
    }

    /// Verdict at a *visible* position (post-filter).
    pub fn visible_verdict_at(&self, vis_index: usize) -> Option<Verdict> {
        self.visible_row(vis_index)
            .map(|r| Verdict::from_action(r.action.as_deref()))
    }

    /// Index (into the full store) of the row with `id`, if present.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.position_of(id)
    }

    /// Position of the row with `id` within the *visible* projection, or `None`
    /// if the id is unknown or currently filtered out. Under no active filter
    /// this equals the full-store index.
    pub fn visible_index_of(&self, id: &str) -> Option<usize> {
        let store_idx = self.position_of(id)?;
        if self.filter.is_active() {
            self.visible.iter().position(|&i| i == store_idx)
        } else {
            Some(store_idx)
        }
    }

    /// Whether the row with `id` is currently pending (awaiting a decision).
    /// `None` when the id is unknown.
    pub fn is_pending(&self, id: &str) -> Option<bool> {
        self.rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.action.is_none())
    }
}

/// Coalesce a sorted, deduped index list into `Remove` ops emitted
/// highest-run-first (so indices remain valid as removals are applied).
fn coalesce_removals(sorted_indices: &[usize]) -> Vec<ModelOp> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &i in sorted_indices {
        match runs.last_mut() {
            Some(run) if run.1 == i => run.1 = i + 1,
            _ => runs.push((i, i + 1)),
        }
    }
    runs.iter()
        .rev()
        .map(|&(start, end)| ModelOp::Remove {
            at: start,
            count: end - start,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: format!("proc-{id}"),
            process_path: None,
            dst_host: format!("host-{id}.example"),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: action.map(|s| s.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    fn ids(store: &RowStore) -> Vec<String> {
        store.rows().iter().map(|r| r.id.clone()).collect()
    }

    #[test]
    fn verdict_maps_action_strings() {
        assert_eq!(Verdict::from_action(None), Verdict::Pending);
        assert_eq!(Verdict::from_action(Some("allow")), Verdict::Allowed);
        assert_eq!(Verdict::from_action(Some("deny")), Verdict::Denied);
        assert_eq!(
            Verdict::from_action(Some("blocklist")),
            Verdict::Blocklisted
        );
        assert_eq!(Verdict::from_action(Some("weird")), Verdict::Other);
        assert_eq!(Verdict::Pending.as_token(), "pending");
    }

    #[test]
    fn insert_appends_in_order_and_reports_range() {
        let mut s = RowStore::new();
        let ops = s.apply(ServerMessage::InsertConnectionRows {
            rows: vec![row("a", Some("allow")), row("b", None)],
        });
        assert_eq!(ops, vec![ModelOp::Insert { at: 0, count: 2 }]);
        assert_eq!(ids(&s), vec!["a", "b"]);

        let ops2 = s.apply(ServerMessage::InsertConnectionRows {
            rows: vec![row("c", Some("deny"))],
        });
        assert_eq!(ops2, vec![ModelOp::Insert { at: 2, count: 1 }]);
        assert_eq!(ids(&s), vec!["a", "b", "c"]);
    }

    #[test]
    fn count_new_ids_matches_actual_append_delta() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None)]);
        // "b" and "c" are new; "a" already exists.
        let batch = vec![row("b", None), row("a", Some("allow")), row("c", None)];
        let predicted = s.count_new_ids(&batch);
        let before = s.len();
        s.insert_rows(batch);
        assert_eq!(predicted, s.len() - before);
        assert_eq!(predicted, 2);
    }

    #[test]
    fn count_new_ids_dedups_duplicate_new_id_within_one_batch() {
        // Regression: a single insert batch carrying the same *new* id twice.
        // insert_rows appends the first occurrence and folds the second into an
        // in-place update, so the real store grows by 1 — not 2. count_new_ids
        // must report exactly that, or the model wrapper's beginInsertRows
        // bracket desyncs from the store and violates a QAbstractListModel
        // invariant.
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None)]);
        let batch = vec![
            row("dup", None),
            row("dup", Some("allow")), // same new id, second time
            row("a", None),            // already present -> update
            row("fresh", None),        // genuinely new
        ];
        let predicted = s.count_new_ids(&batch);
        let before = s.len();
        let ops = s.insert_rows(batch);
        let actual = s.len() - before;

        assert_eq!(
            predicted, actual,
            "count_new_ids must equal insert_rows's real store growth"
        );
        assert_eq!(predicted, 2, "only 'dup' and 'fresh' are genuinely new");
        // The emitted Insert op's count agrees too.
        let inserted: usize = ops
            .iter()
            .map(|op| match op {
                ModelOp::Insert { count, .. } => *count,
                _ => 0,
            })
            .sum();
        assert_eq!(inserted, actual);
        // The second "dup" won: its action is the update-in-place value.
        let dup_idx = s.rows().iter().position(|r| r.id == "dup").unwrap();
        assert_eq!(s.verdict_at(dup_idx), Some(Verdict::Allowed));
    }

    #[test]
    fn count_new_ids_all_existing_is_zero() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None)]);
        assert_eq!(s.count_new_ids(&[row("a", None), row("b", None)]), 0);
    }

    #[test]
    fn insert_of_existing_id_updates_in_place() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None)]);
        let ops = s.insert_rows(vec![row("a", Some("allow")), row("c", None)]);
        // "a" changed in place at index 0; "c" appended at index 2.
        assert_eq!(
            ops,
            vec![
                ModelOp::Changed { indices: vec![0] },
                ModelOp::Insert { at: 2, count: 1 },
            ]
        );
        assert_eq!(ids(&s), vec!["a", "b", "c"]);
        assert_eq!(s.verdict_at(0), Some(Verdict::Allowed));
    }

    #[test]
    fn update_replaces_matched_rows_and_ignores_unknown() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None)]);
        let ops = s.apply(ServerMessage::UpdateConnectionRows {
            rows: vec![row("b", Some("deny")), row("zzz", Some("allow"))],
        });
        assert_eq!(ops, vec![ModelOp::Changed { indices: vec![1] }]);
        assert_eq!(s.verdict_at(1), Some(Verdict::Denied));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn remove_single_row() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
        let ops = s.apply(ServerMessage::RemoveConnectionRows {
            ids: vec!["b".to_string()],
        });
        assert_eq!(ops, vec![ModelOp::Remove { at: 1, count: 1 }]);
        assert_eq!(ids(&s), vec!["a", "c"]);
    }

    #[test]
    fn remove_contiguous_run_is_one_op() {
        let mut s = RowStore::new();
        s.insert_rows(vec![
            row("a", None),
            row("b", None),
            row("c", None),
            row("d", None),
        ]);
        let ops = s.remove_rows(&["b".to_string(), "c".to_string()]);
        assert_eq!(ops, vec![ModelOp::Remove { at: 1, count: 2 }]);
        assert_eq!(ids(&s), vec!["a", "d"]);
    }

    #[test]
    fn remove_non_contiguous_emits_high_index_first() {
        let mut s = RowStore::new();
        s.insert_rows(vec![
            row("a", None),
            row("b", None),
            row("c", None),
            row("d", None),
        ]);
        // remove indices 0 and 2 -> ops for the higher run first so indices stay valid
        let ops = s.remove_rows(&["c".to_string(), "a".to_string()]);
        assert_eq!(
            ops,
            vec![
                ModelOp::Remove { at: 2, count: 1 },
                ModelOp::Remove { at: 0, count: 1 },
            ]
        );
        assert_eq!(ids(&s), vec!["b", "d"]);
    }

    #[test]
    fn remove_unknown_id_is_noop() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None)]);
        let ops = s.remove_rows(&["nope".to_string()]);
        assert!(ops.is_empty());
        assert_eq!(ids(&s), vec!["a"]);
    }

    #[test]
    fn move_rows_to_front_reorders() {
        let mut s = RowStore::new();
        s.insert_rows(vec![
            row("a", None),
            row("b", None),
            row("c", None),
            row("d", None),
        ]);
        let ops = s.apply(ServerMessage::MoveConnetionRows {
            ids: vec!["c".to_string(), "a".to_string()],
        });
        // c and a move to front in the given id order.
        assert_eq!(ids(&s), vec!["c", "a", "b", "d"]);
        // Removed indices 0 (a) and 2 (c) high-first, then inserted 2 at front.
        assert_eq!(
            ops,
            vec![
                ModelOp::Remove { at: 2, count: 1 },
                ModelOp::Remove { at: 0, count: 1 },
                ModelOp::Insert { at: 0, count: 2 },
            ]
        );
    }

    #[test]
    fn move_rows_already_at_front_is_noop() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
        let ops = s.move_rows_to_front(&["a".to_string(), "b".to_string()]);
        assert!(ops.is_empty());
        assert_eq!(ids(&s), vec!["a", "b", "c"]);
    }

    #[test]
    fn remove_range_drains_and_clamps() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None), row("c", None)]);
        s.remove_range(1, 2);
        assert_eq!(ids(&s), vec!["a", "c"]);
        // Out-of-range end is clamped, not a panic.
        s.remove_range(1, 99);
        assert_eq!(ids(&s), vec!["a"]);
    }

    #[test]
    fn clear_resets_and_empty_clear_is_noop() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", None)]);
        let ops = s.apply(ServerMessage::ClearConnectionRows);
        assert_eq!(ops, vec![ModelOp::Reset]);
        assert!(s.is_empty());

        let ops2 = s.clear();
        assert!(ops2.is_empty());
    }

    #[test]
    fn visible_is_identity_when_unfiltered() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", Some("allow"))]);
        assert!(!s.is_filtered());
        assert_eq!(s.visible_len(), 2);
        assert_eq!(s.visible_row(0).unwrap().id, "a");
        assert_eq!(s.visible_row(1).unwrap().id, "b");
    }

    #[test]
    fn pending_only_filter_projects_visible_subset() {
        use crate::connections::filter::ConnectionFilter;
        let mut s = RowStore::new();
        s.insert_rows(vec![
            row("a", None),
            row("b", Some("allow")),
            row("c", None),
        ]);
        let changed = s.set_filter(ConnectionFilter::new("", true));
        assert!(changed);
        assert!(s.is_filtered());
        assert_eq!(s.visible_len(), 2);
        assert_eq!(s.visible_row(0).unwrap().id, "a");
        assert_eq!(s.visible_row(1).unwrap().id, "c");
        assert_eq!(s.visible_verdict_at(0), Some(Verdict::Pending));
    }

    #[test]
    fn setting_identical_filter_reports_no_change() {
        use crate::connections::filter::ConnectionFilter;
        let mut s = RowStore::new();
        assert!(s.set_filter(ConnectionFilter::new("x", false)));
        assert!(!s.set_filter(ConnectionFilter::new("x", false)));
    }

    #[test]
    fn visible_projection_refreshes_after_mutation_when_filtered() {
        use crate::connections::filter::ConnectionFilter;
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None)]);
        s.set_filter(ConnectionFilter::new("", true));
        assert_eq!(s.visible_len(), 1);
        // A decided row arrives; caller recomputes the projection after applying.
        s.insert_rows(vec![row("b", Some("allow"))]);
        s.recompute_visible();
        assert_eq!(s.visible_len(), 1, "decided row excluded by pending_only");
        // Then a new pending row arrives.
        s.insert_rows(vec![row("c", None)]);
        s.recompute_visible();
        assert_eq!(s.visible_len(), 2);
    }

    #[test]
    fn is_pending_and_index_of_reflect_store() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None), row("b", Some("deny"))]);
        assert_eq!(s.index_of("b"), Some(1));
        assert_eq!(s.is_pending("a"), Some(true));
        assert_eq!(s.is_pending("b"), Some(false));
        assert_eq!(s.is_pending("nope"), None);
    }

    #[test]
    fn unrelated_server_message_is_ignored() {
        let mut s = RowStore::new();
        s.insert_rows(vec![row("a", None)]);
        let ops = s.apply(ServerMessage::SetRules { rules: vec![] });
        assert!(ops.is_empty());
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn end_to_end_pending_then_decided_sequence() {
        // Mirrors the core loop: a pending row arrives, then is decided.
        let mut s = RowStore::new();
        s.apply(ServerMessage::InsertConnectionRows {
            rows: vec![row("p1", None)],
        });
        assert_eq!(s.verdict_at(0), Some(Verdict::Pending));

        s.apply(ServerMessage::UpdateConnectionRows {
            rows: vec![row("p1", Some("allow"))],
        });
        assert_eq!(s.verdict_at(0), Some(Verdict::Allowed));
        assert_eq!(s.len(), 1);
    }
}
