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

/// Two-part public suffixes this heuristic special-cases so e.g.
/// `www.example.co.uk` groups under `example.co.uk`, not `co.uk`.
///
/// Not exhaustive (a full Public Suffix List is explicitly out of scope) —
/// this is deliberately a small, documented set of the most common
/// second-level-registration TLDs.
const TWO_PART_TLDS: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "net.uk", "co.jp", "or.jp", "ne.jp", "co.kr", "co.nz",
    "co.za", "co.il", "co.in", "co.id", "com.au", "net.au", "org.au", "com.br", "com.cn", "com.mx",
    "com.sg", "com.tw", "co.th",
];

/// True when `host` parses as a literal IPv4/IPv6 address.
fn is_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

/// Derive a pragmatic "registrable domain" (eTLD+1) from a hostname.
///
/// Heuristic, not a full Public Suffix List implementation (per the design
/// spec, exact PSL handling is not required):
///   * 1-2 labels (`localhost`, `example.com`) — returned as-is.
///   * Last two labels match a known two-part TLD (see [`TWO_PART_TLDS`]) —
///     the last *three* labels are kept (e.g. `example.co.uk`).
///   * Otherwise — the last two labels are kept (e.g. `github.com`).
///
/// Callers are expected to route bare IPs and empty hosts to the IP address
/// instead of this function (see [`domain_key`]) — "bare IPs group under the
/// IP itself" per the design spec.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().to_lowercase();
    if host.is_empty() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() <= 2 {
        return host;
    }
    let last_two = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    if TWO_PART_TLDS.contains(&last_two.as_str()) && labels.len() >= 3 {
        labels[labels.len() - 3..].join(".")
    } else {
        last_two
    }
}

/// The process-group key for `row`: its executable path when known
/// (disambiguates identical process names at different install locations),
/// falling back to the bare process name.
fn process_key(row: &ConnectionRow) -> String {
    row.process_path
        .clone()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| row.process.clone())
}

/// The domain-group key/label for `row`: its registrable domain, or the
/// destination IP for bare-IP / empty hosts.
fn domain_key(row: &ConnectionRow) -> String {
    let host = row.dst_host.trim();
    if host.is_empty() || is_ip(host) {
        row.dst_ip.clone()
    } else {
        registrable_domain(host)
    }
}

/// The host-group key for `row`: its lowercased hostname, or the destination
/// IP for bare-IP / empty hosts (so such a host row equals its domain row).
fn host_key(row: &ConnectionRow) -> String {
    let host = row.dst_host.trim();
    if host.is_empty() || is_ip(host) {
        row.dst_ip.clone()
    } else {
        host.to_lowercase()
    }
}

fn composite_key(process_key: &str, domain_key: &str) -> String {
    format!("{process_key}\u{1}{domain_key}")
}

fn host_composite_key(process_key: &str, domain_key: &str, host_key: &str) -> String {
    format!("{process_key}\u{1}{domain_key}\u{1}{host_key}")
}

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
mod tests {
    use super::*;

    fn row(id: &str, process: &str, host: &str, ip: &str, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: process.to_string(),
            process_path: None,
            dst_host: host.to_string(),
            dst_ip: ip.to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: action.map(|s| s.to_string()),
            bytes_sent: 10,
            bytes_received: 20,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    // --- registrable_domain heuristic ------------------------------------

    #[test]
    fn registrable_domain_strips_subdomains() {
        assert_eq!(registrable_domain("github.com"), "github.com");
        assert_eq!(registrable_domain("www.github.com"), "github.com");
        assert_eq!(registrable_domain("api.foo.bar.github.com"), "github.com");
    }

    #[test]
    fn registrable_domain_handles_two_part_tlds() {
        assert_eq!(registrable_domain("example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("www.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("sub.www.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("shop.example.com.au"), "example.com.au");
    }

    #[test]
    fn registrable_domain_single_label_is_returned_as_is() {
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain(""), "");
    }

    #[test]
    fn registrable_domain_is_case_insensitive() {
        assert_eq!(registrable_domain("WWW.GitHub.COM"), "github.com");
    }

    // --- tree membership/aggregates --------------------------------------

    /// Like [`row`] but with an explicit executable path, for exercising
    /// `process_key`'s exe-path branch (the plain `row` helper always leaves
    /// `process_path: None`, so it only ever exercises the process-name
    /// fallback).
    fn row_with_process_path(
        id: &str,
        process: &str,
        process_path: &str,
        host: &str,
        ip: &str,
        action: Option<&str>,
    ) -> ConnectionRow {
        ConnectionRow {
            process_path: Some(process_path.to_string()),
            ..row(id, process, host, ip, action)
        }
    }

    fn build(rows: &[ConnectionRow]) -> GroupTree {
        let mut t = GroupTree::new();
        for r in rows {
            t.upsert_row(r);
        }
        t
    }

    fn rows_map(rows: &[ConnectionRow]) -> HashMap<&str, &ConnectionRow> {
        rows.iter().map(|r| (r.id.as_str(), r)).collect()
    }

    #[test]
    fn insert_groups_by_process_then_domain() {
        let rows = vec![
            row("a", "firefox", "www.github.com", "1.1.1.1", Some("allow")),
            row("b", "firefox", "api.github.com", "1.1.1.2", None),
            row("c", "firefox", "slack.com", "2.2.2.2", Some("deny")),
            row("d", "slack", "slack.com", "2.2.2.2", Some("allow")),
        ];
        let mut tree = build(&rows);
        assert_eq!(tree.process_count(), 2);
        // Pending rows no longer force expansion; open firefox explicitly so
        // its domain headers are emitted for the grouping assertions below.
        tree.toggle_process("firefox");

        let no_filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &no_filter);

        match &proj[0] {
            VisibleEntry::ProcessHeader {
                key,
                counts,
                expanded,
                ..
            } => {
                assert_eq!(key, "firefox");
                assert_eq!(counts.total, 3);
                assert_eq!(counts.pending, 1);
                assert!(expanded);
            }
            other => panic!("expected firefox ProcessHeader, got {other:?}"),
        }
        // github.com groups a+b under one domain header.
        let github_header = proj
            .iter()
            .find(|e| matches!(e, VisibleEntry::DomainHeader { key, .. } if key == "github.com"));
        match github_header {
            Some(VisibleEntry::DomainHeader { counts, .. }) => assert_eq!(counts.total, 2),
            other => panic!("expected github.com DomainHeader, got {other:?}"),
        }
    }

    #[test]
    fn bare_ip_hosts_group_under_the_ip() {
        let rows = vec![
            row("a", "curl", "", "10.0.0.5", Some("allow")),
            row("b", "curl", "10.0.0.5", "10.0.0.5", Some("allow")),
        ];
        let mut tree = build(&rows);
        tree.toggle_process("curl"); // both rows decided -> expand to inspect
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);
        let domain_headers: Vec<&String> = proj
            .iter()
            .filter_map(|e| match e {
                VisibleEntry::DomainHeader { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(domain_headers, vec![&"10.0.0.5".to_string()]);
    }

    #[test]
    fn update_in_place_moves_counts_not_membership() {
        let mut rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
        let mut tree = build(&rows);
        {
            let filter = ConnectionFilter::default();
            let map = rows_map(&rows);
            let proj = tree.build_projection(&map, &filter);
            // Pending is signalled via the header counts (the badge source).
            match &proj[0] {
                VisibleEntry::ProcessHeader { counts, .. } => assert_eq!(counts.pending, 1),
                other => panic!("unexpected {other:?}"),
            }
        }

        rows[0].action = Some("allow".to_string());
        tree.upsert_row(&rows[0]);
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);
        match &proj[0] {
            VisibleEntry::ProcessHeader {
                counts, expanded, ..
            } => {
                assert_eq!(counts.pending, 0);
                assert_eq!(counts.allowed, 1);
                // No longer forced open (not pending, no filter, not toggled).
                assert!(!expanded);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn update_that_changes_host_moves_row_to_new_domain_group() {
        let mut rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
        let mut tree = build(&rows);
        rows[0].dst_host = "slack.com".to_string();
        rows[0].dst_ip = "2.2.2.2".to_string();
        tree.upsert_row(&rows[0]);
        // After the move (which prunes toggle state for the emptied group),
        // expand the process so its domain headers are emitted for the assert.
        tree.toggle_process("firefox");

        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);
        let domain_keys: Vec<&String> = proj
            .iter()
            .filter_map(|e| match e {
                VisibleEntry::DomainHeader { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(domain_keys, vec![&"slack.com".to_string()]);
    }

    #[test]
    fn remove_prunes_empty_domain_and_process_groups() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
        let mut tree = build(&rows);
        assert_eq!(tree.process_count(), 1);
        tree.remove_row("a");
        assert_eq!(tree.process_count(), 0);

        let filter = ConnectionFilter::default();
        let map: HashMap<&str, &ConnectionRow> = HashMap::new();
        assert!(tree.build_projection(&map, &filter).is_empty());
    }

    #[test]
    fn remove_of_one_row_keeps_sibling_domain_group() {
        let rows = vec![
            row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
            row("b", "firefox", "slack.com", "2.2.2.2", Some("allow")),
        ];
        let mut tree = build(&rows);
        tree.remove_row("a");
        assert_eq!(tree.process_count(), 1);
        tree.toggle_process("firefox"); // decided row -> expand to inspect
        let filter = ConnectionFilter::default();
        let remaining = vec![rows[1].clone()];
        let map = rows_map(&remaining);
        let proj = tree.build_projection(&map, &filter);
        let domain_keys: Vec<&String> = proj
            .iter()
            .filter_map(|e| match e {
                VisibleEntry::DomainHeader { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(domain_keys, vec![&"slack.com".to_string()]);
    }

    #[test]
    fn clear_empties_the_tree() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
        let mut tree = build(&rows);
        tree.clear();
        assert_eq!(tree.process_count(), 0);
    }

    #[test]
    fn expand_collapse_toggles_control_non_pending_groups() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
        let tree_default = build(&rows);
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        // Decided row, not filtered, not toggled -> collapsed by default.
        let proj = tree_default.build_projection(&map, &filter);
        assert_eq!(proj.len(), 1, "process header only, domain+row collapsed");

        let mut tree = build(&rows);
        tree.toggle_process("firefox");
        let proj = tree.build_projection(&map, &filter);
        assert!(
            proj.len() > 1,
            "expanding the process should reveal its domain"
        );

        tree.toggle_domain("firefox", "github.com");
        let proj = tree.build_projection(&map, &filter);
        assert_eq!(proj.len(), 3, "process header + domain header + leaf row");
        match &proj[2] {
            VisibleEntry::Row { id, .. } => assert_eq!(id, "a"),
            other => panic!("expected leaf Row, got {other:?}"),
        }
    }

    #[test]
    fn pending_row_stays_collapsed_and_surfaces_through_header_counts() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
        let mut tree = build(&rows); // never toggled expanded
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);

        let proj = tree.build_projection(&map, &filter);
        assert_eq!(proj.len(), 1, "collapse state wins over pending rows");
        match &proj[0] {
            VisibleEntry::ProcessHeader {
                expanded, counts, ..
            } => {
                assert!(!expanded);
                assert_eq!(counts.pending, 1, "badge count carries the signal");
            }
            other => panic!("expected ProcessHeader, got {other:?}"),
        }

        // Manual expansion still reveals the pending path in full.
        tree.toggle_process("firefox");
        tree.toggle_domain("firefox", "github.com");
        let proj = tree.build_projection(&map, &filter);
        assert_eq!(proj.len(), 3, "process + domain headers + pending leaf");
    }

    #[test]
    fn filter_hides_groups_with_no_matching_descendants_but_shows_matching_ancestors() {
        let rows = vec![
            row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
            row("b", "slack", "slack.com", "2.2.2.2", Some("allow")),
        ];
        let tree = build(&rows);
        let filter = ConnectionFilter::new("github", false);
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);

        // Only the firefox/github.com path survives, and it is forced open
        // by the active filter so the match is actually visible.
        assert_eq!(proj.len(), 3);
        match &proj[0] {
            VisibleEntry::ProcessHeader { key, expanded, .. } => {
                assert_eq!(key, "firefox");
                assert!(expanded);
            }
            other => panic!("unexpected {other:?}"),
        }
        match &proj[2] {
            VisibleEntry::Row { id, .. } => assert_eq!(id, "a"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn move_to_front_reorders_domain_and_process_groups() {
        let rows = vec![
            row("a", "firefox", "github.com", "1.1.1.1", Some("allow")),
            row("b", "slack", "slack.com", "2.2.2.2", Some("allow")),
        ];
        let mut tree = build(&rows);
        // slack was inserted second; move it to front.
        tree.move_to_front(&["b".to_string()]);
        assert_eq!(
            tree.process_order.first().map(String::as_str),
            Some("slack")
        );
    }

    #[test]
    fn move_to_front_of_unknown_id_is_a_no_op() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
        let mut tree = build(&rows);
        tree.move_to_front(&["nope".to_string()]);
        assert_eq!(tree.process_count(), 1);
    }

    // --- pending_row_ids (issue #18 batch actions) ------------------------

    #[test]
    fn pending_row_ids_collects_only_pending_rows_under_a_process_in_display_order() {
        // Insertion order deliberately differs from lexical id/domain order
        // (slack.com before github.com; "c" before "a") so a regression that
        // sorts the result rather than preserving group-then-insertion order
        // would go undetected by a lexically-sorted assertion.
        let rows = vec![
            row("c", "firefox", "slack.com", "2.2.2.2", None),
            row("a", "firefox", "github.com", "1.1.1.1", None),
            row("b", "firefox", "github.com", "1.1.1.1", Some("allow")),
            row("d", "chrome", "slack.com", "2.2.2.2", None),
        ];
        let tree = build(&rows);
        assert_eq!(
            tree.pending_row_ids("firefox"),
            vec!["c".to_string(), "a".to_string()],
            "display order: slack.com (inserted first) then github.com, not lexical"
        );
        assert_eq!(tree.pending_row_ids("chrome"), vec!["d".to_string()]);
    }

    #[test]
    fn pending_row_ids_reflects_move_to_front_reordering() {
        // Two pending rows in the same domain group, inserted "z" then "m"
        // (lexically reversed from insertion order) so the base order is
        // insertion order, not sorted — then move_to_front flips it.
        let rows = vec![
            row("z", "firefox", "github.com", "1.1.1.1", None),
            row("m", "firefox", "github.com", "1.1.1.1", None),
        ];
        let mut tree = build(&rows);
        assert_eq!(
            tree.pending_row_ids("firefox"),
            vec!["z".to_string(), "m".to_string()],
            "insertion order before any move"
        );
        tree.move_to_front(&["m".to_string()]);
        assert_eq!(
            tree.pending_row_ids("firefox"),
            vec!["m".to_string(), "z".to_string()],
            "move_to_front must be reflected in the batch-action id order"
        );
    }

    #[test]
    fn pending_row_ids_uses_executable_path_as_the_process_key_when_present() {
        let rows = vec![row_with_process_path(
            "a",
            "curl",
            "/usr/bin/curl",
            "github.com",
            "1.1.1.1",
            None,
        )];
        let tree = build(&rows);
        // The bare process name is not a valid key once an exe path is set
        // (see `process_key`'s doc comment: exe path disambiguates identical
        // process names at different install locations).
        assert!(tree.pending_row_ids("curl").is_empty());
        assert_eq!(tree.pending_row_ids("/usr/bin/curl"), vec!["a".to_string()]);
    }

    #[test]
    fn remove_row_decrements_pending_counts() {
        // Regression: remove_row used to evict `id_meta` before
        // `remove_from_group` read it back to know which counts to
        // subtract, so `counts.pending` (the batch-button/badge source of
        // truth) never actually decremented on removal.
        let rows = vec![
            row("a", "firefox", "github.com", "1.1.1.1", None),
            row("b", "firefox", "github.com", "1.1.1.1", None),
        ];
        let mut tree = build(&rows);
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        match &tree.build_projection(&map, &filter)[0] {
            VisibleEntry::ProcessHeader { counts, .. } => assert_eq!(counts.pending, 2),
            other => panic!("unexpected {other:?}"),
        }

        tree.remove_row("a");
        assert_eq!(
            tree.pending_row_ids("firefox").len(),
            1,
            "pending_row_ids must drop the removed row"
        );
        let remaining = vec![rows[1].clone()];
        let remaining_map = rows_map(&remaining);
        match &tree.build_projection(&remaining_map, &filter)[0] {
            VisibleEntry::ProcessHeader { counts, .. } => {
                assert_eq!(
                    counts.pending, 1,
                    "counts.pending must decrement on removal"
                )
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn pending_row_ids_len_matches_group_counts_pending_after_mixed_mutations() {
        // Invariant test: pending_row_ids(k).len() must always agree with
        // the aggregate counts.pending for the same group, across a mixed
        // insert/decide/remove sequence — the two are read by different
        // consumers (batch-action buttons vs. the header badge) and must
        // never disagree.
        let mut rows = vec![
            row("a", "firefox", "github.com", "1.1.1.1", None),
            row("b", "firefox", "github.com", "1.1.1.1", None),
            row("c", "firefox", "slack.com", "2.2.2.2", None),
        ];
        let mut tree = build(&rows);
        let assert_invariant = |tree: &GroupTree, rows: &[ConnectionRow]| {
            let filter = ConnectionFilter::default();
            let map = rows_map(rows);
            let proj = tree.build_projection(&map, &filter);
            let counts_pending = match proj.first() {
                Some(VisibleEntry::ProcessHeader { counts, .. }) => counts.pending,
                _ => 0,
            };
            assert_eq!(tree.pending_row_ids("firefox").len(), counts_pending);
        };
        assert_invariant(&tree, &rows);

        // Decide "a".
        rows[0].action = Some("allow".to_string());
        tree.upsert_row(&rows[0]);
        assert_invariant(&tree, &rows);

        // Remove "b" (still pending).
        tree.remove_row("b");
        rows.retain(|r| r.id != "b");
        assert_invariant(&tree, &rows);

        // "c" remains pending; the invariant must still hold.
        assert_eq!(tree.pending_row_ids("firefox"), vec!["c".to_string()]);
    }

    #[test]
    fn pending_row_ids_compose_into_valid_batch_deny_messages() {
        // Pins what `submitBatchVerdict` sends per id for "Deny all":
        // "deny"/"this_host" plus that row's own inline duration (the same
        // `inlineDurationFor` lookup a row's Deny uses) through to a
        // well-formed `SetVerdict`, entirely Qt-free. On a bridge with
        // app-bound rules, a program file it can bind to is denied until
        // restart; no path, once.
        let rows = vec![
            row_with_process_path(
                "a",
                "firefox",
                "/usr/bin/firefox",
                "github.com",
                "1.1.1.1",
                None,
            ),
            row_with_process_path(
                "b",
                "firefox",
                "/usr/bin/firefox",
                "slack.com",
                "2.2.2.2",
                None,
            ),
            row("c", "curl", "github.com", "1.1.1.1", None),
        ];
        let tree = build(&rows);
        let mut store = crate::connections::row_store::RowStore::new();
        store.insert_rows(rows);
        for (process_key, duration) in [("/usr/bin/firefox", "until_restart"), ("curl", "once")] {
            let ids = tree.pending_row_ids(process_key);
            assert!(!ids.is_empty(), "{process_key}");
            for id in ids {
                let token = crate::inline_deny::duration_token_for_row(
                    &store,
                    &id,
                    crate::pending_decision::VerdictChoice::Deny,
                    true,
                    "forever",
                );
                let msg =
                    crate::pending_decision::build_verdict_message(&id, "deny", "this_host", token)
                        .expect("well-formed choice token must build");
                let json = serde_json::to_value(&msg).unwrap();
                assert_eq!(json["rowId"], id);
                assert_eq!(json["verdict"], "deny");
                assert_eq!(json["scope"], "this_host");
                assert_eq!(json["duration"], duration, "{id}");
            }
        }
    }

    #[test]
    fn pending_row_ids_is_empty_for_unknown_or_fully_decided_process() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", Some("allow"))];
        let tree = build(&rows);
        assert!(tree.pending_row_ids("firefox").is_empty());
        assert!(tree.pending_row_ids("nonexistent").is_empty());
    }

    #[test]
    fn pending_row_ids_is_independent_of_collapse_state() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
        let tree = build(&rows); // never expanded
        assert_eq!(tree.pending_row_ids("firefox"), vec!["a".to_string()]);
    }

    // --- host level --------------------------------------------------------

    fn host_rows() -> Vec<ConnectionRow> {
        vec![
            row("a", "firefox", "www.github.com", "1.1.1.1", Some("allow")),
            row("b", "firefox", "api.github.com", "1.1.1.2", None),
            row("c", "firefox", "www.github.com", "1.1.1.1", Some("deny")),
            row("d", "firefox", "slack.com", "2.2.2.2", Some("allow")),
        ]
    }

    fn open_all(tree: &mut GroupTree) {
        tree.set_process_expanded("firefox", true);
        tree.set_domain_expanded("firefox", "github.com", true);
        tree.set_host_expanded("firefox", "github.com", "www.github.com", true);
        tree.set_host_expanded("firefox", "github.com", "api.github.com", true);
    }

    fn shape(proj: &[VisibleEntry]) -> Vec<String> {
        proj.iter()
            .map(|e| match e {
                VisibleEntry::ProcessHeader { key, .. } => format!("P:{key}"),
                VisibleEntry::DomainHeader { key, .. } => format!("D:{key}"),
                VisibleEntry::HostHeader { key, .. } => format!("H:{key}"),
                VisibleEntry::Row { id, host_key, .. } => {
                    format!("R:{id}:{}", host_key.is_some())
                }
            })
            .collect()
    }

    #[test]
    fn host_projection_nests_hosts_between_domain_and_connection() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        assert_eq!(
            shape(&proj),
            vec![
                "P:firefox",
                "D:github.com",
                "H:www.github.com",
                "R:a:true",
                "R:c:true",
                "H:api.github.com",
                "R:b:true",
                "D:slack.com",
            ]
        );
        match &proj[2] {
            VisibleEntry::HostHeader {
                counts, expanded, ..
            } => {
                assert_eq!((counts.total, counts.allowed, counts.denied), (2, 1, 1));
                assert!(expanded);
            }
            other => panic!("expected host header, got {other:?}"),
        }
    }

    #[test]
    fn entry_levels_run_program_domain_host_connection() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        let levels: Vec<i32> = proj.iter().map(VisibleEntry::level).collect();
        assert_eq!(levels, vec![0, 1, 2, 3, 3, 2, 3, 1]);
    }

    #[test]
    fn hosts_are_materialised_only_under_an_expanded_domain() {
        let rows = host_rows();
        let mut tree = build(&rows);
        tree.set_process_expanded("firefox", true);
        // A host flagged open under a collapsed domain stays hidden.
        tree.set_host_expanded("firefox", "github.com", "www.github.com", true);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        assert_eq!(
            shape(&proj),
            vec!["P:firefox", "D:github.com", "D:slack.com"]
        );
    }

    #[test]
    fn domain_and_program_counts_are_the_same_with_or_without_hosts() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let map = rows_map(&rows);
        let filter = ConnectionFilter::default();
        let headers = |proj: Vec<VisibleEntry>| -> Vec<(String, GroupCounts)> {
            proj.into_iter()
                .filter_map(|e| match e {
                    VisibleEntry::ProcessHeader { key, counts, .. }
                    | VisibleEntry::DomainHeader { key, counts, .. } => Some((key, counts)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            headers(tree.build_projection(&map, &filter)),
            headers(tree.build_host_projection(&map, &filter))
        );
    }

    #[test]
    fn host_counts_follow_incremental_updates() {
        let mut rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        // The pending row on api.github.com gets decided.
        rows[1].action = Some("deny".to_string());
        tree.upsert_row(&rows[1]);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        let api = proj.iter().find_map(|e| match e {
            VisibleEntry::HostHeader { key, counts, .. } if key == "api.github.com" => {
                Some(*counts)
            }
            _ => None,
        });
        let api = api.expect("api.github.com host header");
        assert_eq!((api.total, api.pending, api.denied), (1, 0, 1));
    }

    #[test]
    fn the_plain_projection_never_shows_hosts() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let proj = tree.build_projection(&rows_map(&rows), &ConnectionFilter::default());
        assert!(proj
            .iter()
            .all(|e| !matches!(e, VisibleEntry::HostHeader { .. })));
        assert!(proj
            .iter()
            .any(|e| matches!(e, VisibleEntry::Row { host_key: None, .. })));
    }

    #[test]
    fn a_collapsed_host_hides_its_rows_and_keeps_its_counts() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        tree.toggle_host("firefox", "github.com", "www.github.com");
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        let s = shape(&proj);
        assert!(s.contains(&"H:www.github.com".to_string()));
        assert!(!s.contains(&"R:a:true".to_string()));
        assert!(s.contains(&"R:b:true".to_string()));
    }

    #[test]
    fn pending_host_rows_are_signalled_not_forced_open() {
        let rows = host_rows();
        let mut tree = build(&rows);
        tree.set_process_expanded("firefox", true);
        tree.set_domain_expanded("firefox", "github.com", true);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        // api.github.com holds the pending row b but stays collapsed.
        let api = proj
            .iter()
            .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "api.github.com"));
        match api {
            Some(VisibleEntry::HostHeader {
                expanded, counts, ..
            }) => {
                assert!(!expanded);
                assert_eq!(counts.pending, 1);
            }
            other => panic!("expected api.github.com host header, got {other:?}"),
        }
        assert!(!shape(&proj).iter().any(|s| s.starts_with("R:")));
    }

    #[test]
    fn an_active_filter_forces_the_host_path_open_and_hides_other_hosts() {
        let rows = host_rows();
        let tree = build(&rows);
        let filter = ConnectionFilter::new("api.github", false);
        let proj = tree.build_host_projection(&rows_map(&rows), &filter);
        assert_eq!(
            shape(&proj),
            vec!["P:firefox", "D:github.com", "H:api.github.com", "R:b:true"]
        );
    }

    #[test]
    fn hosts_are_pruned_and_counted_down_as_rows_leave() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        tree.remove_row("b");
        tree.remove_row("a");
        let remaining: Vec<ConnectionRow> = rows
            .iter()
            .filter(|r| r.id != "a" && r.id != "b")
            .cloned()
            .collect();
        let proj = tree.build_host_projection(&rows_map(&remaining), &ConnectionFilter::default());
        let s = shape(&proj);
        assert!(!s.contains(&"H:api.github.com".to_string()), "{s:?}");
        match proj
            .iter()
            .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "www.github.com"))
        {
            Some(VisibleEntry::HostHeader { counts, .. }) => assert_eq!(counts.total, 1),
            other => panic!("expected www.github.com, got {other:?}"),
        }
    }

    #[test]
    fn an_update_that_changes_verdict_moves_the_host_counts() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let mut decided = rows.clone();
        decided[1].action = Some("allow".to_string());
        tree.upsert_row(&decided[1]);
        let proj = tree.build_host_projection(&rows_map(&decided), &ConnectionFilter::default());
        match proj
            .iter()
            .find(|e| matches!(e, VisibleEntry::HostHeader { key, .. } if key == "api.github.com"))
        {
            Some(VisibleEntry::HostHeader { counts, .. }) => {
                assert_eq!((counts.pending, counts.allowed), (0, 1));
            }
            other => panic!("expected api.github.com, got {other:?}"),
        }
    }

    #[test]
    fn an_update_to_another_host_moves_the_row_between_hosts() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        let mut moved = rows.clone();
        moved[1].dst_host = "www.github.com".to_string();
        tree.upsert_row(&moved[1]);
        let proj = tree.build_host_projection(&rows_map(&moved), &ConnectionFilter::default());
        let s = shape(&proj);
        assert!(!s.contains(&"H:api.github.com".to_string()), "{s:?}");
    }

    #[test]
    fn bare_ip_hosts_are_keyed_by_the_ip() {
        let rows = vec![row("a", "curl", "", "10.0.0.5", Some("allow"))];
        let mut tree = build(&rows);
        tree.set_process_expanded("curl", true);
        tree.set_domain_expanded("curl", "10.0.0.5", true);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        assert_eq!(shape(&proj), vec!["P:curl", "D:10.0.0.5", "H:10.0.0.5"]);
    }

    #[test]
    fn move_to_front_reorders_hosts_within_a_domain() {
        let rows = host_rows();
        let mut tree = build(&rows);
        open_all(&mut tree);
        tree.move_to_front(&["b".to_string()]);
        let proj = tree.build_host_projection(&rows_map(&rows), &ConnectionFilter::default());
        let hosts: Vec<String> = shape(&proj)
            .into_iter()
            .filter(|s| s.starts_with("H:"))
            .collect();
        assert_eq!(hosts, vec!["H:api.github.com", "H:www.github.com"]);
    }
}
