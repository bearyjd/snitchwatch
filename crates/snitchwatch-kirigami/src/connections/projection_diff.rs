//! Minimal row-range diff between two grouped projections.
//!
//! Expanding or collapsing a group, or changing the filter, used to bracket a
//! full Qt model reset, which drops the view's scroll position and selection
//! and re-creates every delegate. The projection is a flat list where each
//! entry has a stable identity (the group key or the row id), so two
//! projections differ by one contiguous window: everything before it and
//! after it keeps its identity. [`diff`] finds that window, so the model can
//! emit one remove range, one insert range and a `dataChanged` for entries
//! whose content (the expand flag, the counts) changed in place.
//!
//! The cost is proportional to the changed rows, not the whole tree, so
//! collapsing a domain of N rows touches N+1 rows however many are shown.

use std::ops::Range;

use super::grouping::VisibleEntry;

/// How to turn the old projection into the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDiff {
    /// Old-list indices to remove (applied first).
    pub removed: Range<usize>,
    /// New-list indices to insert (applied second; the insertion point is
    /// `inserted.start`, valid once `removed` is gone).
    pub inserted: Range<usize>,
    /// New-list indices whose identity is unchanged but whose content
    /// differs, to report as `dataChanged` after the row ranges are applied.
    pub changed: Vec<usize>,
}

impl ProjectionDiff {
    /// Rows the model has to signal about; the O(changed) bound the
    /// performance test pins.
    pub fn touched(&self) -> usize {
        self.removed.len() + self.inserted.len() + self.changed.len()
    }
}

fn same_identity(a: &VisibleEntry, b: &VisibleEntry) -> bool {
    use VisibleEntry::*;
    match (a, b) {
        (ProcessHeader { key: a, .. }, ProcessHeader { key: b, .. }) => a == b,
        (
            DomainHeader {
                process_key: ap,
                key: a,
                ..
            },
            DomainHeader {
                process_key: bp,
                key: b,
                ..
            },
        ) => ap == bp && a == b,
        (
            HostHeader {
                process_key: ap,
                domain_key: ad,
                key: a,
                ..
            },
            HostHeader {
                process_key: bp,
                domain_key: bd,
                key: b,
                ..
            },
        ) => ap == bp && ad == bd && a == b,
        (Row { id: a, .. }, Row { id: b, .. }) => a == b,
        _ => false,
    }
}

/// Diff `old` against `new` by shared identity prefix and suffix.
pub fn diff(old: &[VisibleEntry], new: &[VisibleEntry]) -> ProjectionDiff {
    let max = old.len().min(new.len());
    let prefix = (0..max)
        .take_while(|&i| same_identity(&old[i], &new[i]))
        .count();
    let suffix = (0..max - prefix)
        .take_while(|&j| same_identity(&old[old.len() - 1 - j], &new[new.len() - 1 - j]))
        .count();

    let prefix_changed = (0..prefix).filter(|&i| old[i] != new[i]);
    let suffix_changed =
        (new.len() - suffix..new.len()).filter(|&i| old[i + old.len() - new.len()] != new[i]);
    ProjectionDiff {
        removed: prefix..old.len() - suffix,
        inserted: prefix..new.len() - suffix,
        changed: prefix_changed.chain(suffix_changed).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::filter::ConnectionFilter;
    use crate::connections::grouping::GroupTree;
    use snitchwatch_bridge::ws_messages::ConnectionRow;
    use std::collections::HashMap;

    fn row(id: usize, process: usize, domain: usize, host: usize) -> ConnectionRow {
        ConnectionRow {
            id: format!("r{id}"),
            process: format!("proc{process}"),
            process_path: None,
            dst_host: format!("h{host}.d{domain}.example"),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: Some("allow".to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    /// 5 programs x 10 domains x 10 hosts x 10 connections = 5000 rows.
    fn rows_5k() -> Vec<ConnectionRow> {
        let mut rows = Vec::new();
        for p in 0..5 {
            for d in 0..10 {
                for h in 0..10 {
                    for _ in 0..10 {
                        rows.push(row(rows.len(), p, d, h));
                    }
                }
            }
        }
        rows
    }

    fn tree_of(rows: &[ConnectionRow]) -> GroupTree {
        let mut tree = GroupTree::new();
        for r in rows {
            tree.upsert_row(r);
        }
        tree
    }

    fn project(tree: &GroupTree, rows: &[ConnectionRow]) -> Vec<VisibleEntry> {
        let map: HashMap<&str, &ConnectionRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
        tree.build_host_projection(&map, &ConnectionFilter::default())
    }

    /// Replay `diff` on `old` the way the model does and return the result.
    fn apply(old: &[VisibleEntry], new: &[VisibleEntry], d: &ProjectionDiff) -> Vec<VisibleEntry> {
        let mut out = old.to_vec();
        out.drain(d.removed.clone());
        for (offset, entry) in new[d.inserted.clone()].iter().enumerate() {
            out.insert(d.inserted.start + offset, entry.clone());
        }
        for &i in &d.changed {
            out[i] = new[i].clone();
        }
        out
    }

    fn open_everything(tree: &mut GroupTree) {
        for p in 0..5 {
            let pk = format!("proc{p}");
            tree.set_process_expanded(&pk, true);
            for d in 0..10 {
                let dk = format!("d{d}.example");
                tree.set_domain_expanded(&pk, &dk, true);
                for h in 0..10 {
                    tree.set_host_expanded(&pk, &dk, &format!("h{h}.d{d}.example"), true);
                }
            }
        }
    }

    #[test]
    fn identical_projections_have_an_empty_diff() {
        let rows = rows_5k();
        let tree = tree_of(&rows);
        let proj = project(&tree, &rows);
        let d = diff(&proj, &proj);
        assert_eq!(d.touched(), 0);
    }

    #[test]
    fn collapsing_one_domain_of_5k_rows_touches_only_its_descendants() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let before = project(&tree, &rows);
        // 5 programs + 50 domains + 500 hosts + 5000 connections.
        assert_eq!(before.len(), 5555);

        tree.toggle_domain("proc2", "d4.example");
        let after = project(&tree, &rows);

        let d = diff(&before, &after);
        // The domain's 10 hosts and 100 connections go; the header changes.
        assert_eq!(d.removed.len(), 110);
        assert_eq!(d.inserted.len(), 0);
        assert_eq!(d.changed.len(), 1);
        assert!(d.touched() <= 110 + 1, "touched {}", d.touched());
        assert_eq!(apply(&before, &after, &d), after);
    }

    #[test]
    fn toggling_one_host_of_5k_rows_is_a_ranged_change_of_its_connections() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let before = project(&tree, &rows);

        tree.toggle_host("proc1", "d3.example", "h7.d3.example");
        let after = project(&tree, &rows);

        let d = diff(&before, &after);
        // Its 10 connections go; the host header repaints. No reset.
        assert_eq!(
            (d.removed.len(), d.inserted.len(), d.changed.len()),
            (10, 0, 1)
        );
        assert_eq!(apply(&before, &after, &d), after);

        let back = {
            tree.toggle_host("proc1", "d3.example", "h7.d3.example");
            project(&tree, &rows)
        };
        let d = diff(&after, &back);
        assert_eq!(
            (d.removed.len(), d.inserted.len(), d.changed.len()),
            (0, 10, 1)
        );
    }

    #[test]
    fn expanding_is_the_mirror_image_of_collapsing() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        tree.set_domain_expanded("proc0", "d0.example", false);
        let before = project(&tree, &rows);
        tree.toggle_domain("proc0", "d0.example");
        let after = project(&tree, &rows);

        let d = diff(&before, &after);
        assert_eq!(
            (d.removed.len(), d.inserted.len(), d.changed.len()),
            (0, 110, 1)
        );
        assert_eq!(apply(&before, &after, &d), after);
    }

    #[test]
    fn a_count_change_in_place_is_a_data_change_not_a_row_change() {
        let mut rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let before = project(&tree, &rows);
        rows[0].action = None; // pending
        tree.upsert_row(&rows[0]);
        let after = project(&tree, &rows);

        let d = diff(&before, &after);
        assert_eq!((d.removed.len(), d.inserted.len()), (0, 0));
        // program, domain and host headers above row 0 changed counts.
        assert_eq!(d.changed.len(), 3);
        assert_eq!(apply(&before, &after, &d), after);
    }

    #[test]
    fn a_filter_change_replaces_one_window_and_replays_exactly() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let map: HashMap<&str, &ConnectionRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
        let before = tree.build_host_projection(&map, &ConnectionFilter::default());
        let filter = ConnectionFilter::new("h3.d7.example", false);
        let after = tree.build_host_projection(&map, &filter);

        let d = diff(&before, &after);
        assert_eq!(apply(&before, &after, &d), after);
        assert!(d.touched() < before.len() + after.len());
    }

    #[test]
    fn replay_matches_for_every_domain_toggle_in_one_program() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let mut current = project(&tree, &rows);
        for d in 0..10 {
            tree.toggle_domain("proc1", &format!("d{d}.example"));
            let next = project(&tree, &rows);
            let step = diff(&current, &next);
            assert_eq!(apply(&current, &next, &step), next);
            current = next;
        }
    }

    /// The app's own projection (program > domain > connection, no host level)
    /// at 5k rows: collapsing a domain removes exactly its 100 connections.
    #[test]
    fn collapsing_one_domain_in_the_apps_projection_of_5k_rows_replays_exactly() {
        let rows = rows_5k();
        let mut tree = tree_of(&rows);
        open_everything(&mut tree);
        let map: HashMap<&str, &ConnectionRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
        let before = tree.build_projection(&map, &ConnectionFilter::default());
        // 5 programs + 50 domains + 5000 connections.
        assert_eq!(before.len(), 5055);

        tree.toggle_domain("proc2", "d4.example");
        let after = tree.build_projection(&map, &ConnectionFilter::default());

        let d = diff(&before, &after);
        assert_eq!(
            (d.removed.len(), d.inserted.len(), d.changed.len()),
            (100, 0, 1)
        );
        assert_eq!(apply(&before, &after, &d), after);
    }
}
