//! Process → Domain grouping layer over [`super::row_store::RowStore`]
//! (Little-Snitch-parity Task: "Process→Domain grouping in Connections
//! monitor").
//!
//! This is a second, Qt-free projection sitting alongside the existing flat
//! `RowStore` visible-row projection. It mirrors the same insert/update/
//! remove/move/clear deltas the row store applies (see
//! `ConnectionsModel::apply_server_message`, which feeds both) and maintains
//! a two-level tree — process, then registrable domain — with running
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
//!     wrapper brackets a call to this with a Qt model reset, matching the
//!     precedent it already sets for the filtered-flat path ("a filter is an
//!     explicit user investigation mode... a reset is acceptable here").
//!
//! Design decision — pending rows always visible: rather than track
//! "auto-expanded because a pending row arrived" as separate mutable state
//! (which would need explicit collapse-back logic once the row is decided),
//! [`build_projection`](GroupTree::build_projection) treats a group as
//! effectively expanded whenever it has at least one pending descendant,
//! regardless of the user's toggled state. This satisfies both "pending rows
//! always visible/highlighted within their group" and "auto-expand the path
//! to a new pending row" with one rule, and naturally lets the group return
//! to the user's chosen collapsed state once the row is decided.
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

fn composite_key(process_key: &str, domain_key: &str) -> String {
    format!("{process_key}\u{1}{domain_key}")
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
struct DomainGroup {
    label: String,
    /// Row ids in display order (front = most recently active, mirroring
    /// `RowStore::move_rows_to_front`'s move-to-front semantics).
    row_order: Vec<String>,
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
    Row {
        id: String,
        process_key: String,
        domain_key: String,
    },
}

/// The incrementally-maintained Process → Domain group tree.
#[derive(Debug, Default)]
pub struct GroupTree {
    process_order: Vec<String>,
    processes: HashMap<String, ProcessGroup>,
    id_meta: HashMap<String, RowMeta>,
    expanded_processes: HashSet<String>,
    expanded_domains: HashSet<String>,
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
        let verdict = Verdict::from_action(row.action.as_deref());
        let pkey = process_key(row);
        let dkey = domain_key(row);
        let bytes_sent = row.bytes_sent;
        let bytes_received = row.bytes_received;

        match self.id_meta.get(&row.id).cloned() {
            Some(old) if old.process_key == pkey && old.domain_key == dkey => {
                self.with_counts(&pkey, &dkey, |c| {
                    c.sub(old.verdict, old.bytes_sent, old.bytes_received);
                    c.add(verdict, bytes_sent, bytes_received);
                });
                self.relabel(&pkey, &dkey, row);
            }
            Some(old) => {
                self.remove_from_group(&old.process_key, &old.domain_key, &row.id);
                self.insert_into_group(&pkey, &dkey, row);
            }
            None => {
                self.insert_into_group(&pkey, &dkey, row);
            }
        }

        self.id_meta.insert(
            row.id.clone(),
            RowMeta {
                process_key: pkey,
                domain_key: dkey,
                verdict,
                bytes_sent,
                bytes_received,
            },
        );
    }

    /// Remove a row by id, pruning now-empty domain/process groups.
    pub fn remove_row(&mut self, id: &str) {
        if let Some(meta) = self.id_meta.remove(id) {
            self.remove_from_group(&meta.process_key, &meta.domain_key, id);
        }
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

    /// Number of process groups currently tracked (test/debug helper).
    pub fn process_count(&self) -> usize {
        self.process_order.len()
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

            let process_expanded =
                self.expanded_processes.contains(pkey) || pg.counts.pending > 0 || filter_active;
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
                let domain_expanded = self.expanded_domains.contains(&composite)
                    || dg.counts.pending > 0
                    || filter_active;
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
                for id in visible_ids {
                    out.push(VisibleEntry::Row {
                        id: id.clone(),
                        process_key: pkey.clone(),
                        domain_key: dkey.clone(),
                    });
                }
            }
        }

        out
    }

    // --- internal helpers ---------------------------------------------

    fn insert_into_group(&mut self, pkey: &str, dkey: &str, row: &ConnectionRow) {
        let verdict = Verdict::from_action(row.action.as_deref());
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
    }

    fn remove_from_group(&mut self, pkey: &str, dkey: &str, id: &str) {
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

    fn with_counts(&mut self, pkey: &str, dkey: &str, f: impl Fn(&mut GroupCounts)) {
        if let Some(pg) = self.processes.get_mut(pkey) {
            f(&mut pg.counts);
            if let Some(dg) = pg.domains.get_mut(dkey) {
                f(&mut dg.counts);
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
        let tree = build(&rows);
        assert_eq!(tree.process_count(), 2);

        let no_filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &no_filter);

        // firefox has a pending row (b) -> process + its domains force-expand.
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
            // Pending forces expansion.
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
    fn pending_row_forces_group_expansion_even_when_collapsed() {
        let rows = vec![row("a", "firefox", "github.com", "1.1.1.1", None)];
        let tree = build(&rows); // never toggled expanded
        let filter = ConnectionFilter::default();
        let map = rows_map(&rows);
        let proj = tree.build_projection(&map, &filter);
        assert_eq!(proj.len(), 3, "pending row forces the whole path open");
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
}
