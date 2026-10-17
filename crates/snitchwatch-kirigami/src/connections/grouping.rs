//! Process → Domain grouping layer over [`super::row_store::RowStore`]
//! (Little-Snitch-parity Task: "Process→Domain grouping in Connections
//! monitor").
//!
//! This is a second, Qt-free projection sitting alongside the existing flat
//! `RowStore` visible-row projection. It mirrors the same insert/update/
//! remove/move/clear deltas the row store applies (see
//! `ConnectionsModel::apply_server_message`, which feeds both) and maintains
//! a three-level tree — process, registrable domain, then host — with running
//! aggregate counts, incrementally:
//!
//!   * [`GroupTree::upsert_row`] / [`remove_row`](GroupTree::remove_row) /
//!     [`move_to_front`](GroupTree::move_to_front) /
//!     [`clear`](GroupTree::clear) update group membership and per-group
//!     [`GroupCounts`] in O(1)-ish time per changed row — no rescan of the
//!     whole row set. This is the part the design spec's "aggregates update
//!     incrementally... don't rebuild the whole tree per message" refers to.
//!   * [`GroupTree::build_projection`] *derives* a flat, display-ready list
//!     of [`VisibleEntry`] (group headers + leaf rows) from that maintained
//!     tree, honouring expand/collapse state and the active
//!     [`ConnectionFilter`]. This flatten step is proportional to the
//!     visible tree, not a re-derivation of group membership/aggregates —
//!     the same "recompute the view from maintained state" shape the
//!     `cxx-qt` model wrapper already uses elsewhere (e.g. `refresh_counts`
//!     scans `store.rows()` after every mutation). The `ConnectionsModel`
//!     wrapper brackets a call to this with a Qt model reset when rows
//!     arrive, but swaps in the result with ranged row signals for an
//!     expand/collapse or a filter change (see [`super::projection_diff`]).
//!
//! Design decision — pending work is signalled, never forced open (issue
//! #20): a group with pending descendants stays in whatever expand/collapse
//! state the user chose; the header's pending count (rendered as a badge in
//! `ConnectionsPage.qml`) is the signal that undecided rows are hidden
//! beneath it. An earlier iteration force-expanded any group with a pending
//! descendant, but on a busy system that override fired on every rebuild
//! and made manual collapse impossible. The active filter still forces the
//! path to a match open — a filter is an explicit user investigation mode,
//! and a hidden match would read as "no results".
//!
//! Design decision — the host level: the tree always tracks hosts, and
//! [`GroupTree::build_host_projection`] (what the shell renders) puts host rows
//! between domain and connection. Hosts are materialised lazily, only under an
//! expanded domain. [`GroupTree::build_projection`] keeps the two-level
//! program > domain > connection shape for callers that do not want hosts.
//!
//! Design decision — domain grouping heuristic: exact Public Suffix List
//! handling is explicitly not required. [`registrable_domain`] uses a
//! pragmatic heuristic (last two labels, with a small hardcoded set of
//! common two-part TLDs such as `co.uk`/`com.au`) — see its doc comment.

use std::collections::{HashMap, HashSet};

use super::filter::ConnectionFilter;
use super::row_store::Verdict;
use snitchwatch_bridge::ws_messages::ConnectionRow;

mod keys;

pub use keys::registrable_domain;
use keys::{composite_key, domain_key, host_composite_key, host_key, process_key};

/// Running aggregate counts for a group (process or domain level).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupCounts {
    pub total: usize,
    pub pending: usize,
    pub allowed: usize,
    pub denied: usize,
    pub blocklisted: usize,
    pub other: usize,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl GroupCounts {
    fn add(&mut self, verdict: Verdict, bytes_sent: u64, bytes_received: u64) {
        self.total += 1;
        self.bytes_sent += bytes_sent;
        self.bytes_received += bytes_received;
        match verdict {
            Verdict::Pending => self.pending += 1,
            Verdict::Allowed => self.allowed += 1,
            Verdict::Denied => self.denied += 1,
            Verdict::Blocklisted => self.blocklisted += 1,
            Verdict::Other => self.other += 1,
        }
    }

    fn sub(&mut self, verdict: Verdict, bytes_sent: u64, bytes_received: u64) {
        self.total = self.total.saturating_sub(1);
        self.bytes_sent = self.bytes_sent.saturating_sub(bytes_sent);
        self.bytes_received = self.bytes_received.saturating_sub(bytes_received);
        match verdict {
            Verdict::Pending => self.pending = self.pending.saturating_sub(1),
            Verdict::Allowed => self.allowed = self.allowed.saturating_sub(1),
            Verdict::Denied => self.denied = self.denied.saturating_sub(1),
            Verdict::Blocklisted => self.blocklisted = self.blocklisted.saturating_sub(1),
            Verdict::Other => self.other = self.other.saturating_sub(1),
        }
    }
}

#[derive(Debug, Default)]
struct HostGroup {
    /// Row ids in display order, same ordering rule as [`DomainGroup::row_order`].
    row_order: Vec<String>,
    counts: GroupCounts,
}

#[derive(Debug, Default)]
struct DomainGroup {
    label: String,
    /// Row ids in display order (front = most recently active, mirroring
    /// `RowStore::move_rows_to_front`'s move-to-front semantics).
    row_order: Vec<String>,
    host_order: Vec<String>,
    hosts: HashMap<String, HostGroup>,
    counts: GroupCounts,
}

#[derive(Debug, Default)]
struct ProcessGroup {
    label: String,
    domain_order: Vec<String>,
    domains: HashMap<String, DomainGroup>,
    counts: GroupCounts,
}

/// Cached per-row metadata needed to reverse a row's group membership and to
/// compute count deltas on update/remove without re-deriving it from a
/// `ConnectionRow` the tree doesn't otherwise retain.
#[derive(Debug, Clone)]
struct RowMeta {
    process_key: String,
    domain_key: String,
    host_key: String,
    verdict: Verdict,
    bytes_sent: u64,
    bytes_received: u64,
}

/// One row of the flattened, display-ready grouped projection.
#[derive(Debug, Clone, PartialEq)]
pub enum VisibleEntry {
    ProcessHeader {
        key: String,
        label: String,
        expanded: bool,
        counts: GroupCounts,
    },
    DomainHeader {
        process_key: String,
        key: String,
        label: String,
        expanded: bool,
        counts: GroupCounts,
    },
    /// A host under a domain; only emitted by
    /// [`GroupTree::build_host_projection`].
    HostHeader {
        process_key: String,
        domain_key: String,
        key: String,
        label: String,
        expanded: bool,
        counts: GroupCounts,
    },
    Row {
        id: String,
        process_key: String,
        domain_key: String,
        /// `Some` when the row sits under a host header (host projection).
        host_key: Option<String>,
    },
}

impl VisibleEntry {
    /// The tier this entry sits on, as the model's `groupLevel` role reports
    /// it: 0 program, 1 domain, 2 host, 3 connection.
    pub fn level(&self) -> i32 {
        match self {
            VisibleEntry::ProcessHeader { .. } => 0,
            VisibleEntry::DomainHeader { .. } => 1,
            VisibleEntry::HostHeader { .. } => 2,
            VisibleEntry::Row { .. } => 3,
        }
    }
}

/// The incrementally-maintained Process → Domain group tree.
#[derive(Debug, Default)]
pub struct GroupTree {
    process_order: Vec<String>,
    processes: HashMap<String, ProcessGroup>,
    id_meta: HashMap<String, RowMeta>,
    expanded_processes: HashSet<String>,
    expanded_domains: HashSet<String>,
    expanded_hosts: HashSet<String>,
}

impl GroupTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// The host keys currently under a program's domain group.
    pub fn domain_hosts<'a>(
        &'a self,
        process_key: &str,
        domain_key: &str,
    ) -> impl Iterator<Item = &'a str> {
        self.processes
            .get(process_key)
            .and_then(|p| p.domains.get(domain_key))
            .into_iter()
            .flat_map(|d| d.host_order.iter().map(String::as_str))
    }

    /// Insert a new row or update an existing one (matched by id), keeping
    /// group membership and aggregate counts in sync. Mirrors
    /// `RowStore::insert_rows`/`update_rows`'s "existing id = update in
    /// place" semantics.
    pub fn upsert_row(&mut self, row: &ConnectionRow) {
        let verdict = Verdict::of(row);
        let pkey = process_key(row);
        let dkey = domain_key(row);
        let hkey = host_key(row);
        let bytes_sent = row.bytes_sent;
        let bytes_received = row.bytes_received;

        match self.id_meta.get(&row.id).cloned() {
            Some(old)
                if old.process_key == pkey && old.domain_key == dkey && old.host_key == hkey =>
            {
                self.with_counts(&pkey, &dkey, &hkey, |c| {
                    c.sub(old.verdict, old.bytes_sent, old.bytes_received);
                    c.add(verdict, bytes_sent, bytes_received);
                });
                self.relabel(&pkey, &dkey, row);
            }
            Some(old) => {
                self.remove_from_group(&old.process_key, &old.domain_key, &old.host_key, &row.id);
                self.insert_into_group(&pkey, &dkey, &hkey, row);
            }
            None => {
                self.insert_into_group(&pkey, &dkey, &hkey, row);
            }
        }

        self.id_meta.insert(
            row.id.clone(),
            RowMeta {
                process_key: pkey,
                domain_key: dkey,
                host_key: hkey,
                verdict,
                bytes_sent,
                bytes_received,
            },
        );
    }

    /// Remove a row by id, pruning now-empty domain/process groups.
    ///
    /// Meta must be read (and cloned) *before* `remove_from_group` runs, not
    /// removed first: `remove_from_group` looks the row back up in
    /// `id_meta` to know which counts to subtract, so evicting the entry up
    /// front would make every removal a silent no-op on the aggregate
    /// counts (a real regression this fix addresses — see the
    /// `remove_row_decrements_pending_counts` test).
    pub fn remove_row(&mut self, id: &str) {
        let Some(meta) = self.id_meta.get(id).cloned() else {
            return;
        };
        self.remove_from_group(&meta.process_key, &meta.domain_key, &meta.host_key, id);
        self.id_meta.remove(id);
    }

    /// Move the identified rows to the front of their domain group (and
    /// bubble their domain/process to the front of their own siblings),
    /// mirroring `RowStore::move_rows_to_front`'s "most-recent-first"
    /// semantics inside the grouped view.
    pub fn move_to_front(&mut self, ids: &[String]) {
        // Reverse order so the final front-most id (processed last) ends up
        // truly first, matching `ids`' intended priority order.
        for id in ids.iter().rev() {
            let Some(meta) = self.id_meta.get(id).cloned() else {
                continue;
            };
            if let Some(pg) = self.processes.get_mut(&meta.process_key) {
                if let Some(dg) = pg.domains.get_mut(&meta.domain_key) {
                    if let Some(pos) = dg.row_order.iter().position(|r| r == id) {
                        let row = dg.row_order.remove(pos);
                        dg.row_order.insert(0, row);
                    }
                    if let Some(hg) = dg.hosts.get_mut(&meta.host_key) {
                        if let Some(pos) = hg.row_order.iter().position(|r| r == id) {
                            let row = hg.row_order.remove(pos);
                            hg.row_order.insert(0, row);
                        }
                    }
                    if let Some(pos) = dg.host_order.iter().position(|h| h == &meta.host_key) {
                        let h = dg.host_order.remove(pos);
                        dg.host_order.insert(0, h);
                    }
                }
                if let Some(pos) = pg.domain_order.iter().position(|d| d == &meta.domain_key) {
                    let d = pg.domain_order.remove(pos);
                    pg.domain_order.insert(0, d);
                }
            }
            if let Some(pos) = self
                .process_order
                .iter()
                .position(|p| p == &meta.process_key)
            {
                let p = self.process_order.remove(pos);
                self.process_order.insert(0, p);
            }
        }
    }

    /// Drop all groups and cached row metadata.
    pub fn clear(&mut self) {
        self.process_order.clear();
        self.processes.clear();
        self.id_meta.clear();
        self.expanded_processes.clear();
        self.expanded_domains.clear();
        self.expanded_hosts.clear();
    }

    pub fn toggle_process(&mut self, key: &str) {
        if !self.expanded_processes.remove(key) {
            self.expanded_processes.insert(key.to_string());
        }
    }

    pub fn set_process_expanded(&mut self, key: &str, expanded: bool) {
        if expanded {
            self.expanded_processes.insert(key.to_string());
        } else {
            self.expanded_processes.remove(key);
        }
    }

    pub fn toggle_domain(&mut self, process_key: &str, domain_key: &str) {
        let composite = composite_key(process_key, domain_key);
        if !self.expanded_domains.remove(&composite) {
            self.expanded_domains.insert(composite);
        }
    }

    pub fn set_domain_expanded(&mut self, process_key: &str, domain_key: &str, expanded: bool) {
        let composite = composite_key(process_key, domain_key);
        if expanded {
            self.expanded_domains.insert(composite);
        } else {
            self.expanded_domains.remove(&composite);
        }
    }

    pub fn toggle_host(&mut self, process_key: &str, domain_key: &str, host_key: &str) {
        let composite = host_composite_key(process_key, domain_key, host_key);
        if !self.expanded_hosts.remove(&composite) {
            self.expanded_hosts.insert(composite);
        }
    }

    pub fn set_host_expanded(
        &mut self,
        process_key: &str,
        domain_key: &str,
        host_key: &str,
        expanded: bool,
    ) {
        let composite = host_composite_key(process_key, domain_key, host_key);
        if expanded {
            self.expanded_hosts.insert(composite);
        } else {
            self.expanded_hosts.remove(&composite);
        }
    }

    /// Open the host holding row `id`, but only when its program and domain
    /// are both already open (a collapse there is the user's call and stays).
    /// Returns true when the host's state changed.
    pub fn open_host_of_row_under_open_domain(&mut self, id: &str) -> bool {
        let Some(meta) = self.id_meta.get(id) else {
            return false;
        };
        if !self.expanded_processes.contains(&meta.process_key)
            || !self
                .expanded_domains
                .contains(&composite_key(&meta.process_key, &meta.domain_key))
        {
            return false;
        }
        let composite = host_composite_key(&meta.process_key, &meta.domain_key, &meta.host_key);
        self.expanded_hosts.insert(composite)
    }

    /// Number of process groups currently tracked (test/debug helper).
    pub fn process_count(&self) -> usize {
        self.process_order.len()
    }

    /// Ids of every row still pending a decision under process group
    /// `process_key`, in display order (domain-group order, then row order
    /// within each domain) — the batch-action seam for issue #18's
    /// "Allow all"/"Deny all" process-header buttons. Independent of expand/
    /// collapse state (a collapsed group's pending rows are exactly what the
    /// batch action needs to reach) and of the active filter (a batch action
    /// on a header should not silently skip filtered-out rows).
    pub fn pending_row_ids(&self, process_key: &str) -> Vec<String> {
        let Some(pg) = self.processes.get(process_key) else {
            return Vec::new();
        };
        pg.domain_order
            .iter()
            .filter_map(|dkey| pg.domains.get(dkey))
            .flat_map(|dg| dg.row_order.iter())
            .filter(|id| {
                self.id_meta
                    .get(id.as_str())
                    .map(|meta| meta.verdict == Verdict::Pending)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Build the flattened, display-ready projection: process headers, then
    /// (if expanded) domain headers, then (if expanded) leaf rows — in
    /// group-then-insertion order.
    ///
    /// `rows_by_id` must resolve every id currently held by the tree to its
    /// `ConnectionRow` (the caller — `RowStore::rows()` — is the single
    /// source of truth for row content; the tree only tracks membership +
    /// aggregates). A group is hidden entirely when the filter is active and
    /// none of its descendants match; a group's header is still emitted
    /// (with only matching descendants beneath it) whenever at least one
    /// descendant matches, so ancestor headers stay visible for filtered
    /// children per the design spec.
    pub fn build_projection(
        &self,
        rows_by_id: &HashMap<&str, &ConnectionRow>,
        filter: &ConnectionFilter,
    ) -> Vec<VisibleEntry> {
        self.project(rows_by_id, filter, false)
    }

    /// Like [`build_projection`](Self::build_projection) but with the host
    /// level: program > domain > host > connection. Under an expanded domain
    /// each host is a header (rows only appear under an expanded host, or
    /// when the filter forces the path open).
    pub fn build_host_projection(
        &self,
        rows_by_id: &HashMap<&str, &ConnectionRow>,
        filter: &ConnectionFilter,
    ) -> Vec<VisibleEntry> {
        self.project(rows_by_id, filter, true)
    }

    fn project(
        &self,
        rows_by_id: &HashMap<&str, &ConnectionRow>,
        filter: &ConnectionFilter,
        with_hosts: bool,
    ) -> Vec<VisibleEntry> {
        let filter_active = filter.is_active();
        let mut out = Vec::new();

        for pkey in &self.process_order {
            let Some(pg) = self.processes.get(pkey) else {
                continue;
            };

            let mut visible_domains: Vec<(&String, Vec<&String>)> = Vec::new();
            for dkey in &pg.domain_order {
                let Some(dg) = pg.domains.get(dkey) else {
                    continue;
                };
                let visible_ids: Vec<&String> = dg
                    .row_order
                    .iter()
                    .filter(|id| match rows_by_id.get(id.as_str()) {
                        Some(row) => !filter_active || filter.matches(row),
                        None => false,
                    })
                    .collect();
                if filter_active && visible_ids.is_empty() {
                    continue;
                }
                visible_domains.push((dkey, visible_ids));
            }
            if filter_active && visible_domains.is_empty() {
                continue;
            }

            let process_expanded = self.expanded_processes.contains(pkey) || filter_active;
            out.push(VisibleEntry::ProcessHeader {
                key: pkey.clone(),
                label: pg.label.clone(),
                expanded: process_expanded,
                counts: pg.counts,
            });
            if !process_expanded {
                continue;
            }

            for (dkey, visible_ids) in visible_domains {
                let dg = &pg.domains[dkey];
                let composite = composite_key(pkey, dkey);
                let domain_expanded = self.expanded_domains.contains(&composite) || filter_active;
                out.push(VisibleEntry::DomainHeader {
                    process_key: pkey.clone(),
                    key: dkey.clone(),
                    label: dg.label.clone(),
                    expanded: domain_expanded,
                    counts: dg.counts,
                });
                if !domain_expanded {
                    continue;
                }
                if with_hosts {
                    self.push_hosts(&mut out, pkey, dkey, dg, &visible_ids, filter_active);
                    continue;
                }
                for id in visible_ids {
                    out.push(VisibleEntry::Row {
                        id: id.clone(),
                        process_key: pkey.clone(),
                        domain_key: dkey.clone(),
                        host_key: None,
                    });
                }
            }
        }

        out
    }

    // --- internal helpers ---------------------------------------------

    /// Host headers (and their rows) for one expanded domain. `visible_ids`
    /// is the domain's filter-surviving row set; a host with none of them is
    /// hidden while the filter is active.
    fn push_hosts(
        &self,
        out: &mut Vec<VisibleEntry>,
        pkey: &str,
        dkey: &str,
        dg: &DomainGroup,
        visible_ids: &[&String],
        filter_active: bool,
    ) {
        let visible: HashSet<&str> = visible_ids.iter().map(|id| id.as_str()).collect();
        for hkey in &dg.host_order {
            let Some(hg) = dg.hosts.get(hkey) else {
                continue;
            };
            let host_ids: Vec<&String> = hg
                .row_order
                .iter()
                .filter(|id| visible.contains(id.as_str()))
                .collect();
            if filter_active && host_ids.is_empty() {
                continue;
            }
            let expanded = filter_active
                || self
                    .expanded_hosts
                    .contains(&host_composite_key(pkey, dkey, hkey));
            out.push(VisibleEntry::HostHeader {
                process_key: pkey.to_string(),
                domain_key: dkey.to_string(),
                key: hkey.clone(),
                label: hkey.clone(),
                expanded,
                counts: hg.counts,
            });
            if !expanded {
                continue;
            }
            for id in host_ids {
                out.push(VisibleEntry::Row {
                    id: id.clone(),
                    process_key: pkey.to_string(),
                    domain_key: dkey.to_string(),
                    host_key: Some(hkey.clone()),
                });
            }
        }
    }

    fn insert_into_group(&mut self, pkey: &str, dkey: &str, hkey: &str, row: &ConnectionRow) {
        let verdict = Verdict::of(row);
        if !self.processes.contains_key(pkey) {
            self.process_order.push(pkey.to_string());
            self.processes.insert(
                pkey.to_string(),
                ProcessGroup {
                    label: row.process.clone(),
                    ..Default::default()
                },
            );
        }
        let pg = self.processes.get_mut(pkey).expect("just inserted");
        if !pg.domains.contains_key(dkey) {
            pg.domain_order.push(dkey.to_string());
            pg.domains.insert(
                dkey.to_string(),
                DomainGroup {
                    label: dkey.to_string(),
                    ..Default::default()
                },
            );
        }
        pg.counts.add(verdict, row.bytes_sent, row.bytes_received);
        let dg = pg.domains.get_mut(dkey).expect("just inserted");
        dg.row_order.push(row.id.clone());
        dg.counts.add(verdict, row.bytes_sent, row.bytes_received);
        if !dg.hosts.contains_key(hkey) {
            dg.host_order.push(hkey.to_string());
            dg.hosts.insert(hkey.to_string(), HostGroup::default());
        }
        let hg = dg.hosts.get_mut(hkey).expect("just inserted");
        hg.row_order.push(row.id.clone());
        hg.counts.add(verdict, row.bytes_sent, row.bytes_received);
    }

    fn remove_from_group(&mut self, pkey: &str, dkey: &str, hkey: &str, id: &str) {
        let Some(pg) = self.processes.get_mut(pkey) else {
            return;
        };
        let mut domain_now_empty = false;
        if let Some(dg) = pg.domains.get_mut(dkey) {
            if let Some(pos) = dg.row_order.iter().position(|r| r == id) {
                dg.row_order.remove(pos);
                if let Some(meta) = self.id_meta.get(id) {
                    dg.counts
                        .sub(meta.verdict, meta.bytes_sent, meta.bytes_received);
                    pg.counts
                        .sub(meta.verdict, meta.bytes_sent, meta.bytes_received);
                }
            }
            let mut host_now_empty = false;
            if let Some(hg) = dg.hosts.get_mut(hkey) {
                if let Some(pos) = hg.row_order.iter().position(|r| r == id) {
                    hg.row_order.remove(pos);
                    if let Some(meta) = self.id_meta.get(id) {
                        hg.counts
                            .sub(meta.verdict, meta.bytes_sent, meta.bytes_received);
                    }
                }
                host_now_empty = hg.row_order.is_empty();
            }
            if host_now_empty {
                dg.hosts.remove(hkey);
                dg.host_order.retain(|h| h != hkey);
                self.expanded_hosts
                    .remove(&host_composite_key(pkey, dkey, hkey));
            }
            domain_now_empty = dg.row_order.is_empty();
        }
        if domain_now_empty {
            pg.domains.remove(dkey);
            pg.domain_order.retain(|d| d != dkey);
            self.expanded_domains.remove(&composite_key(pkey, dkey));
        }
        if pg.domains.is_empty() {
            self.processes.remove(pkey);
            self.process_order.retain(|p| p != pkey);
            self.expanded_processes.remove(pkey);
        }
    }

    fn with_counts(&mut self, pkey: &str, dkey: &str, hkey: &str, f: impl Fn(&mut GroupCounts)) {
        if let Some(pg) = self.processes.get_mut(pkey) {
            f(&mut pg.counts);
            if let Some(dg) = pg.domains.get_mut(dkey) {
                f(&mut dg.counts);
                if let Some(hg) = dg.hosts.get_mut(hkey) {
                    f(&mut hg.counts);
                }
            }
        }
    }

    fn relabel(&mut self, pkey: &str, dkey: &str, row: &ConnectionRow) {
        if let Some(pg) = self.processes.get_mut(pkey) {
            pg.label = row.process.clone();
            if let Some(dg) = pg.domains.get_mut(dkey) {
                dg.label = dkey.to_string();
            }
        }
    }
}

#[cfg(test)]
mod host_tests;
#[cfg(test)]
mod tests;
