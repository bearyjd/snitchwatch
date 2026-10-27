//! Per-row updates of the visible projection while the only narrowing is the
//! exception-first collapse (0c). A row delta then re-decides that one row
//! instead of the whole store ([`RowStore::recompute_visible`]), so the flat
//! view keeps ranged Qt signals: plan a step (read-only), signal it, commit it.

use super::*;

/// Where one row change moves the visible projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleChange {
    /// Not visible before or after.
    None,
    /// Visible at this position before and after.
    Changed(usize),
    /// Becomes visible at this position.
    Inserted(usize),
    /// Was visible at this position.
    Removed(usize),
}

/// One planned row step; `index` is the row's store index (`None` for a
/// new row, appended at the end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowStep {
    pub index: Option<usize>,
    pub change: VisibleChange,
}

impl RowStore {
    /// Whether a delta of this filter can be applied per row.
    pub fn is_incremental(&self) -> bool {
        self.filter.narrows_only_by_collapse()
    }

    /// Plan inserting or replacing `row` (an existing id is replaced in
    /// place, as [`Self::insert_rows`] does).
    pub fn plan_upsert(&self, row: &ConnectionRow) -> RowStep {
        let index = self.position_of(&row.id);
        let now = self.filter.matches(row);
        let change = match index.map(|i| self.visible.binary_search(&i)) {
            None if now => VisibleChange::Inserted(self.visible.len()),
            None => VisibleChange::None,
            Some(Ok(at)) if now => VisibleChange::Changed(at),
            Some(Ok(at)) => VisibleChange::Removed(at),
            Some(Err(at)) if now => VisibleChange::Inserted(at),
            Some(Err(_)) => VisibleChange::None,
        };
        RowStep { index, change }
    }

    pub fn commit_upsert(&mut self, step: RowStep, row: ConnectionRow) {
        let index = match step.index {
            Some(index) => {
                self.rows[index] = row;
                index
            }
            None => {
                self.rows.push(row);
                self.rows.len() - 1
            }
        };
        match step.change {
            VisibleChange::Inserted(at) => self.visible.insert(at, index),
            VisibleChange::Removed(at) => {
                self.visible.remove(at);
            }
            VisibleChange::Changed(_) | VisibleChange::None => {}
        }
    }

    /// Plan removing the row `id`; `None` when it is unknown.
    pub fn plan_remove(&self, id: &str) -> Option<RowStep> {
        let index = self.position_of(id)?;
        let change = match self.visible.binary_search(&index) {
            Ok(at) => VisibleChange::Removed(at),
            Err(_) => VisibleChange::None,
        };
        Some(RowStep {
            index: Some(index),
            change,
        })
    }

    pub fn commit_remove(&mut self, step: RowStep) {
        let Some(index) = step.index else {
            return;
        };
        self.rows.remove(index);
        if let VisibleChange::Removed(at) = step.change {
            self.visible.remove(at);
        }
        let later = self.visible.partition_point(|&i| i < index);
        for i in &mut self.visible[later..] {
            *i -= 1;
        }
    }

    /// Apply `msg` row by row with no Qt signals (the grouped view, which
    /// redraws its own projection). Move and clear take the full path.
    pub fn apply_incremental(&mut self, msg: ServerMessage) {
        match msg {
            ServerMessage::InsertConnectionRows { rows } => {
                for row in rows {
                    let step = self.plan_upsert(&row);
                    self.commit_upsert(step, row);
                }
            }
            ServerMessage::UpdateConnectionRows { rows } => {
                for row in rows {
                    let step = self.plan_upsert(&row);
                    // An update for a row already gone is ignored.
                    if step.index.is_some() {
                        self.commit_upsert(step, row);
                    }
                }
            }
            ServerMessage::RemoveConnectionRows { ids } => {
                for id in ids {
                    if let Some(step) = self.plan_remove(&id) {
                        self.commit_remove(step);
                    }
                }
            }
            other => {
                self.apply(other);
                self.recompute_visible();
            }
        }
    }

    /// How many times the whole store was re-filtered.
    pub fn full_recompute_count(&self) -> usize {
        self.full_recomputes
    }
}

#[cfg(test)]
mod tests;
