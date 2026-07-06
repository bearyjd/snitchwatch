//! Per-country aggregate store — the Geo panel's equivalent of
//! `connections::row_store::RowStore`.
//!
//! Unlike the connections list (an ordered list of individual rows), the geo
//! panel is a *aggregate* keyed by country bucket: each incoming connection
//! row increments a running total/allowed/denied/pending count for its
//! resolved bucket, and removal/decision-change decrements/adjusts it. We
//! track each tracked connection id's current bucket + verdict so an update
//! or removal can find and undo its exact prior contribution — the same
//! id-addressable-by-position idea `RowStore` uses, just keyed by id directly
//! since there's no visible per-row ordering to maintain here.
//!
//! `blocklist`-actioned rows count toward `denied` (a blocklist match is a
//! deny outcome from the user's point of view); any other unrecognised
//! action string counts toward `total` only, matching `Verdict::Other`'s
//! "decided but unrecognised" semantics in `row_store`.

use std::collections::HashMap;
use std::sync::Arc;

use snitchwatch_bridge::ws_messages::{ConnectionRow, ServerMessage};

use super::resolver::{Bucket, CountryLookup, SharedResolver};
use crate::connections::row_store::Verdict;

/// One row of the materialised, sorted display list the QML `ListView` binds
/// to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRow {
    /// Uppercase ISO alpha-2 code, or empty for the "Local network" /
    /// "Unknown" buckets (they have no ISO code).
    pub country_code: String,
    pub country_name: String,
    /// Empty when [`super::flag::flag_emoji`] has no glyph for this bucket
    /// (the two special buckets use a neutral marker instead — see
    /// [`bucket_flag`]).
    pub flag: String,
    pub total: i32,
    pub allowed: i32,
    pub denied: i32,
    pub pending: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Aggregate {
    country_name: String,
    total: i32,
    allowed: i32,
    denied: i32,
    pending: i32,
}

impl Aggregate {
    fn add(&mut self, verdict: Verdict) {
        self.total += 1;
        match verdict {
            Verdict::Allowed => self.allowed += 1,
            Verdict::Denied | Verdict::Blocklisted => self.denied += 1,
            Verdict::Pending => self.pending += 1,
            Verdict::Other => {}
        }
    }

    fn remove(&mut self, verdict: Verdict) {
        self.total = self.total.saturating_sub(1);
        match verdict {
            Verdict::Allowed => self.allowed = self.allowed.saturating_sub(1),
            Verdict::Denied | Verdict::Blocklisted => self.denied = self.denied.saturating_sub(1),
            Verdict::Pending => self.pending = self.pending.saturating_sub(1),
            Verdict::Other => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }
}

/// Per-country aggregate of connection rows, fed the same connection-row
/// `ServerMessage` variants `RowStore` consumes.
///
/// Resolution goes through a [`SharedResolver`] rather than an owned
/// lookup/cache pair: `GeoModel::start_bridge_feed`'s Tokio task holds a
/// clone of the *same* resolver and resolves each row's destination IP
/// before the message ever reaches this store (see that module's docs) — the
/// actual GeoIP database read happens off the Qt thread. By the time
/// `apply`/`add_row` calls `resolver.resolve(ip)` here, it's a warm cache hit.
pub struct GeoStore {
    resolver: SharedResolver,
    /// Tracked connection id -> (bucket, verdict at time of last apply), so
    /// update/remove can find and undo the row's exact prior contribution.
    tracked: HashMap<String, (Bucket, Verdict)>,
    aggregates: HashMap<Bucket, Aggregate>,
}

impl Default for GeoStore {
    fn default() -> Self {
        Self::with_resolver(SharedResolver::default())
    }
}

impl GeoStore {
    /// Convenience constructor for tests and simple call sites that don't
    /// need to share the resolver (and its cache) with anything else.
    pub fn new(lookup: Option<Arc<dyn CountryLookup>>) -> Self {
        Self::with_resolver(SharedResolver::new(lookup))
    }

    /// Construct from an existing [`SharedResolver`] — used by `GeoModel` so
    /// its live feed task and this store share one resolver/cache instance.
    pub fn with_resolver(resolver: SharedResolver) -> Self {
        Self {
            resolver,
            tracked: HashMap::new(),
            aggregates: HashMap::new(),
        }
    }

    /// Number of distinct buckets currently populated.
    pub fn bucket_count(&self) -> usize {
        self.aggregates.len()
    }

    /// Apply one bridge `ServerMessage`. Returns `true` if any aggregate
    /// changed (so the cxx-qt wrapper knows whether a model reset is
    /// needed). Only the connection-row variants mutate the store.
    pub fn apply(&mut self, msg: &ServerMessage) -> bool {
        match msg {
            ServerMessage::InsertConnectionRows { rows } => self.insert_rows(rows),
            ServerMessage::UpdateConnectionRows { rows } => self.update_rows(rows),
            ServerMessage::RemoveConnectionRows { ids } => self.remove_rows(ids),
            ServerMessage::MoveConnetionRows { .. } => false, // reordering only; no count change
            ServerMessage::ClearConnectionRows => self.clear(),
            _ => false,
        }
    }

    fn insert_rows(&mut self, rows: &[ConnectionRow]) -> bool {
        let mut changed = false;
        for row in rows {
            // An "insert" of an id we already track is really an update (the
            // bridge can re-send a row it already sent) — undo the old
            // contribution first, exactly like `RowStore::insert_rows`.
            if let Some((old_bucket, old_verdict)) = self.tracked.remove(&row.id) {
                self.decrement(&old_bucket, old_verdict);
            }
            self.add_row(row);
            changed = true;
        }
        changed
    }

    fn update_rows(&mut self, rows: &[ConnectionRow]) -> bool {
        let mut changed = false;
        for row in rows {
            // Unknown ids are ignored — an update for a row we've already
            // evicted is not an error, matching `RowStore::update_rows`.
            if let Some((old_bucket, old_verdict)) = self.tracked.remove(&row.id) {
                self.decrement(&old_bucket, old_verdict);
                self.add_row(row);
                changed = true;
            }
        }
        changed
    }

    fn remove_rows(&mut self, ids: &[String]) -> bool {
        let mut changed = false;
        for id in ids {
            if let Some((bucket, verdict)) = self.tracked.remove(id) {
                self.decrement(&bucket, verdict);
                changed = true;
            }
        }
        changed
    }

    fn clear(&mut self) -> bool {
        let changed = !self.tracked.is_empty();
        self.tracked.clear();
        self.aggregates.clear();
        changed
    }

    fn add_row(&mut self, row: &ConnectionRow) {
        let resolved = self.resolver.resolve(&row.dst_ip);
        let verdict = Verdict::from_action(row.action.as_deref());
        self.aggregates
            .entry(resolved.bucket.clone())
            .or_insert_with(|| Aggregate {
                country_name: resolved.display_name.clone(),
                ..Aggregate::default()
            })
            .add(verdict);
        self.tracked
            .insert(row.id.clone(), (resolved.bucket, verdict));
    }

    fn decrement(&mut self, bucket: &Bucket, verdict: Verdict) {
        if let Some(agg) = self.aggregates.get_mut(bucket) {
            agg.remove(verdict);
            if agg.is_empty() {
                self.aggregates.remove(bucket);
            }
        }
    }

    /// Materialise the current aggregates as display rows, sorted by total
    /// descending (ties broken alphabetically by name for stable ordering).
    pub fn display_rows(&self) -> Vec<DisplayRow> {
        let mut rows: Vec<DisplayRow> = self
            .aggregates
            .iter()
            .map(|(bucket, agg)| DisplayRow {
                country_code: bucket_code(bucket),
                country_name: agg.country_name.clone(),
                flag: bucket_flag(bucket),
                total: agg.total,
                allowed: agg.allowed,
                denied: agg.denied,
                pending: agg.pending,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.total
                .cmp(&a.total)
                .then_with(|| a.country_name.cmp(&b.country_name))
        });
        rows
    }
}

fn bucket_code(bucket: &Bucket) -> String {
    match bucket {
        Bucket::Country(code) => code.clone(),
        Bucket::Local | Bucket::Unknown => String::new(),
    }
}

fn bucket_flag(bucket: &Bucket) -> String {
    match bucket {
        Bucket::Country(code) => super::flag::flag_emoji(code).unwrap_or_default(),
        Bucket::Local => "\u{1F3E0}".to_string(), // house (local network)
        Bucket::Unknown => "\u{2753}".to_string(), // question mark (unresolved)
    }
}

#[cfg(test)]
mod tests {
    use super::super::resolver::fakes::FakeLookup;
    use super::*;

    fn row(id: &str, ip: &str, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: format!("proc-{id}"),
            process_path: None,
            dst_host: format!("host-{id}.example"),
            dst_ip: ip.to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: action.map(|s| s.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
        }
    }

    fn store_with_us_lookup() -> GeoStore {
        let lookup = FakeLookup::default()
            .with("140.82.121.4", "US", "United States")
            .with("104.16.132.229", "US", "United States")
            .with("81.2.69.142", "GB", "United Kingdom");
        GeoStore::new(Some(Arc::new(lookup)))
    }

    #[test]
    fn insert_creates_bucket_with_counts() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].country_code, "US");
        assert_eq!(rows[0].country_name, "United States");
        assert_eq!(rows[0].total, 1);
        assert_eq!(rows[0].pending, 1);
        assert_eq!(rows[0].allowed, 0);
        assert_eq!(rows[0].denied, 0);
    }

    #[test]
    fn multiple_rows_same_country_aggregate() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "140.82.121.4", Some("allow")),
                row("b", "104.16.132.229", Some("deny")),
                row("c", "140.82.121.4", None),
            ],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].total, 3);
        assert_eq!(rows[0].allowed, 1);
        assert_eq!(rows[0].denied, 1);
        assert_eq!(rows[0].pending, 1);
    }

    #[test]
    fn different_countries_produce_separate_buckets() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "140.82.121.4", Some("allow")),
                row("b", "81.2.69.142", Some("allow")),
            ],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 2);
        let codes: Vec<&str> = rows.iter().map(|r| r.country_code.as_str()).collect();
        assert!(codes.contains(&"US"));
        assert!(codes.contains(&"GB"));
    }

    #[test]
    fn sorted_by_total_descending() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "81.2.69.142", None),
                row("b", "140.82.121.4", None),
                row("c", "140.82.121.4", None),
            ],
        });
        let rows = s.display_rows();
        assert_eq!(rows[0].country_code, "US"); // 2 connections
        assert_eq!(rows[1].country_code, "GB"); // 1 connection
    }

    #[test]
    fn private_ip_lands_in_local_bucket() {
        let mut s = GeoStore::default();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "192.168.1.5", None)],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].country_name, "Local network");
        assert_eq!(rows[0].country_code, "");
        assert_eq!(rows[0].flag, "\u{1F3E0}");
    }

    #[test]
    fn public_ip_without_db_lands_in_unknown_bucket() {
        let mut s = GeoStore::default(); // no lookup configured
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "8.8.8.8", None)],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].country_name, "Unknown");
        assert_eq!(rows[0].flag, "\u{2753}");
    }

    #[test]
    fn update_moves_row_between_verdict_counts_without_changing_bucket() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        s.apply(&ServerMessage::UpdateConnectionRows {
            rows: vec![row("a", "140.82.121.4", Some("allow"))],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].total, 1);
        assert_eq!(rows[0].pending, 0);
        assert_eq!(rows[0].allowed, 1);
    }

    #[test]
    fn update_of_unknown_id_is_ignored() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        let changed = s.apply(&ServerMessage::UpdateConnectionRows {
            rows: vec![row("zzz", "140.82.121.4", Some("allow"))],
        });
        assert!(!changed);
        assert_eq!(s.display_rows()[0].total, 1);
    }

    #[test]
    fn remove_decrements_and_evicts_empty_bucket() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "140.82.121.4", None),
                row("b", "81.2.69.142", None),
            ],
        });
        s.apply(&ServerMessage::RemoveConnectionRows {
            ids: vec!["a".to_string()],
        });
        let rows = s.display_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].country_code, "GB");
    }

    #[test]
    fn remove_unknown_id_is_noop() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        let changed = s.apply(&ServerMessage::RemoveConnectionRows {
            ids: vec!["nope".to_string()],
        });
        assert!(!changed);
        assert_eq!(s.display_rows()[0].total, 1);
    }

    #[test]
    fn clear_empties_all_buckets() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "140.82.121.4", None),
                row("b", "81.2.69.142", None),
            ],
        });
        let changed = s.apply(&ServerMessage::ClearConnectionRows);
        assert!(changed);
        assert!(s.display_rows().is_empty());
        assert_eq!(s.bucket_count(), 0);
    }

    #[test]
    fn clear_on_empty_store_is_noop() {
        let mut s = GeoStore::default();
        assert!(!s.apply(&ServerMessage::ClearConnectionRows));
    }

    #[test]
    fn move_rows_does_not_change_counts() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        let changed = s.apply(&ServerMessage::MoveConnetionRows {
            ids: vec!["a".to_string()],
        });
        assert!(!changed);
        assert_eq!(s.display_rows()[0].total, 1);
    }

    #[test]
    fn unrelated_server_message_is_ignored() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        let changed = s.apply(&ServerMessage::SetRules { rules: vec![] });
        assert!(!changed);
        assert_eq!(s.display_rows()[0].total, 1);
    }

    #[test]
    fn ip_cache_avoids_repeat_lookups() {
        // A lookup that only answers once; a second call for the same IP
        // would panic, proving the cache prevented it.
        struct OnceLookup(std::sync::atomic::AtomicBool);
        impl CountryLookup for OnceLookup {
            fn lookup(
                &self,
                addr: std::net::IpAddr,
            ) -> Option<super::super::resolver::CountryInfo> {
                assert!(
                    !self.0.swap(true, std::sync::atomic::Ordering::SeqCst),
                    "lookup called more than once for the same IP"
                );
                let expected: std::net::IpAddr = "140.82.121.4".parse().unwrap();
                assert_eq!(addr, expected);
                Some(super::super::resolver::CountryInfo {
                    code: "US".to_string(),
                    name: "United States".to_string(),
                })
            }
        }
        let mut s = GeoStore::new(Some(Arc::new(OnceLookup(
            std::sync::atomic::AtomicBool::new(false),
        ))));
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![
                row("a", "140.82.121.4", None),
                row("b", "140.82.121.4", None),
                row("c", "140.82.121.4", None),
            ],
        });
        assert_eq!(s.display_rows()[0].total, 3);
    }

    #[test]
    fn with_resolver_shares_cache_with_an_external_clone() {
        // Simulates the live-feed shape: the feed task's resolver clone
        // resolves the IP first (as `start_bridge_feed` does before
        // queueing), then the store built from another clone of that same
        // resolver must hit the already-warm cache, never calling `lookup`
        // itself.
        struct OnceLookup(std::sync::atomic::AtomicBool);
        impl CountryLookup for OnceLookup {
            fn lookup(
                &self,
                _addr: std::net::IpAddr,
            ) -> Option<super::super::resolver::CountryInfo> {
                assert!(
                    !self.0.swap(true, std::sync::atomic::Ordering::SeqCst),
                    "lookup called more than once across the shared resolver"
                );
                Some(super::super::resolver::CountryInfo {
                    code: "US".to_string(),
                    name: "United States".to_string(),
                })
            }
        }
        let resolver = super::super::resolver::SharedResolver::new(Some(Arc::new(OnceLookup(
            std::sync::atomic::AtomicBool::new(false),
        ))));
        // Feed task warms the cache for this IP first.
        resolver.resolve("140.82.121.4");

        let mut s = GeoStore::with_resolver(resolver);
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", None)],
        });
        assert_eq!(s.display_rows()[0].country_code, "US");
    }

    #[test]
    fn blocklisted_verdict_counts_as_denied() {
        let mut s = store_with_us_lookup();
        s.apply(&ServerMessage::InsertConnectionRows {
            rows: vec![row("a", "140.82.121.4", Some("blocklist"))],
        });
        let rows = s.display_rows();
        assert_eq!(rows[0].denied, 1);
        assert_eq!(rows[0].total, 1);
    }
}
