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

/// Friendly, never-blank label for a row's rule-match diagnostics (Little-
/// Snitch-parity "which rule decided this connection" surfacing).
///
/// * A pending row (no verdict yet) reads "— awaiting decision" — there is no
///   rule to point to yet, and a blank field would look broken rather than
///   "not applicable yet".
/// * A decided row with a known `matched_rule` shows that rule's name
///   verbatim (the inspector's "Show rule" action navigates to it).
/// * A row the bridge marks `decided_by_default` (E3: the daemon reported
///   that no rule matched) names the firewall's default action and the
///   action it applied, whatever `matched_rule` says.
/// * A decided row with no rule name on record (e.g. a row that predates this
///   field, or a decision path that doesn't yet populate it) reads "default
///   action" rather than blank — the connection was still decided by
///   *something* (opensnitchd's default action), even though the bridge
///   doesn't have a specific rule name for it.
pub fn matched_rule_display(row: &ConnectionRow) -> String {
    if super::outcome::is_pending(row) {
        return "— awaiting decision".to_string();
    }
    if row.decided_by_default {
        return match row.action.as_deref() {
            Some(action @ ("allow" | "deny")) => {
                format!("No rule: the firewall's default action ({action})")
            }
            _ => "No rule: the firewall's default action".to_string(),
        };
    }
    match row.matched_rule.as_deref() {
        Some(name) if !name.trim().is_empty() => name.to_string(),
        _ => "default action".to_string(),
    }
}

/// The rule name "Show rule" and the 5-minute-block note read (the
/// `matchedRule` role): empty when no rule decided the row
/// (`decided_by_default`, E3), whatever `matched_rule` says, so the flag
/// always means "decided, no rule" (PR #108 review).
pub fn matched_rule_name(row: &ConnectionRow) -> &str {
    if row.decided_by_default {
        return "";
    }
    row.matched_rule.as_deref().unwrap_or("")
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
    /// How many times `visible` was rebuilt from every row (0c's per-row
    /// path, `incremental`, must not add to it).
    full_recomputes: usize,
}

impl RowStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store whose filter is `filter` (carrying its country resolver
    /// from the start; [`Self::set_filter`] compares by value and so would not
    /// swap in a resolver alone).
    pub fn with_filter(filter: ConnectionFilter) -> Self {
        Self {
            filter,
            ..Self::default()
        }
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

    /// opensnitchd caps a single `AskRule` at 120s and unconditionally clears
    /// its serialization flag once that fires (`vendor/opensnitch/daemon/ui/
    /// client.go:366`). Bridges since #39 remove the row when that deadline
    /// cancels the `AskRule` (`grpc_server.rs` `PendingCleanup`), but an
    /// older bridge (the published v0.1.1 tarball) has no reaper for a
    /// cancelled/dropped verdict oneshot, so a row whose `AskRule` already
    /// timed out upstream can stay "pending" in this store indefinitely.
    /// Excluded from [`oldest_pending_started_at_ms`] for exactly that
    /// reason — see its doc comment.
    const ASK_RULE_TIMEOUT_MS: i64 = 120_000;

    /// `started_at_ms` of the longest-*actively*-pending row still awaiting a
    /// decision, across the whole store (independent of the active filter —
    /// mirrors `pending_count`'s scope). `None` when nothing is actively
    /// pending. Feeds the pending-decision-exposure warning (see
    /// docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md):
    /// while any one `AskRule` is outstanding, opensnitchd silently defaults
    /// every *other* concurrent connection, so the oldest pending row is what
    /// determines how long that exposure window has been open.
    ///
    /// Deliberately excludes rows older than [`Self::ASK_RULE_TIMEOUT_MS`]
    /// (opensnitchd's own `AskRule` deadline): a naive "just take the oldest
    /// pending row" picks up a permanently-stuck row whose *real* exposure
    /// window already closed upstream, which — because that row's age
    /// exceeds the caller's ceiling check too — hides the warning entirely
    /// for a second, genuinely active pending row that arrives afterward.
    /// `now_ms` is caller-supplied (not `SystemTime::now()`) to keep this
    /// method pure and Qt-free/unit-testable, matching the rest of this file.
    pub fn oldest_pending_started_at_ms(&self, now_ms: i64) -> Option<i64> {
        self.rows
            .iter()
            .filter(|r| super::outcome::is_pending(r))
            .map(|r| r.started_at_ms)
            .filter(|&started| now_ms - started < Self::ASK_RULE_TIMEOUT_MS)
            .min()
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
        self.rows.get(index).map(Verdict::of)
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

    /// Install off-thread facts without changing the user's filter identity.
    pub fn merge_prepared_geo(
        &mut self,
        reset: bool,
        facts: std::collections::HashMap<String, crate::geo::resolver::Resolved>,
    ) {
        if reset {
            self.filter.clear_prepared_geo();
        }
        self.filter.merge_prepared_geo(facts);
    }
    /// Prune only after the complete grouped batch; later buffered inserts
    /// may already have supplied facts before an earlier Clear is applied.
    pub fn prune_prepared_geo(&mut self, absorbed: impl Fn(&str) -> bool) {
        let ids: std::collections::HashSet<_> =
            self.rows.iter().map(|row| row.id.as_str()).collect();
        self.filter
            .retain_prepared_geo(|id| ids.contains(id) || absorbed(id));
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

    /// Replace what the filter's "handled" check reads (0c) and recompute
    /// `visible`: unlike [`Self::set_filter`], never a no-op.
    pub fn set_attention_facts(&mut self, facts: std::sync::Arc<super::attention::AttentionFacts>) {
        self.filter.set_attention_facts(facts);
        self.recompute_visible();
    }

    /// Recompute `visible` from the current filter. No-op-ish when inactive
    /// (clears `visible`; the visible list is then identity over `rows`).
    pub fn recompute_visible(&mut self) {
        if !self.filter.is_active() {
            self.visible.clear();
            return;
        }
        self.full_recomputes += 1;
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
        self.visible_row(vis_index).map(Verdict::of)
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
            .map(super::outcome::is_pending)
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

mod incremental;
pub use incremental::{RowStep, VisibleChange};

#[cfg(test)]
mod tests;
