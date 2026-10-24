//! Bounded off-thread geographic preparation, independent of cache residency.
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use snitchwatch_bridge::ws_messages::ServerMessage;

use super::resolver::{Bucket, Resolved, SharedResolver};
use super::store::MAX_TRACKED_ROWS;

pub struct PreparedGeoMessage {
    pub message: Option<ServerMessage>,
    pub resolutions: HashMap<String, Resolved>,
    pub reset: bool,
    pub capacity_reached: bool,
    pub activity_codes: Vec<String>,
}

impl PreparedGeoMessage {
    /// Match ConnectionsModel's existing session-qualified live IDs. Call only
    /// when the input to prepare() used raw bridge IDs; already-qualified
    /// callers keep their IDs unchanged.
    pub fn qualify_ids(&mut self, connection_id: u64) {
        if let Some(message) = &mut self.message {
            match message {
                ServerMessage::InsertConnectionRows { rows }
                | ServerMessage::UpdateConnectionRows { rows } => {
                    for row in rows {
                        row.id = format!("{connection_id}:{}", row.id);
                    }
                }
                ServerMessage::RemoveConnectionRows { ids } => {
                    for id in ids {
                        *id = format!("{connection_id}:{id}");
                    }
                }
                _ => {}
            }
        }
        self.resolutions = std::mem::take(&mut self.resolutions)
            .into_iter()
            .map(|(id, resolved)| (format!("{connection_id}:{id}"), resolved))
            .collect();
    }
}

pub struct GeoMessagePreparer {
    resolver: SharedResolver,
    session: Option<u64>,
    accepted: HashSet<String>,
    capacity: usize,
    capacity_reached: bool,
}

impl GeoMessagePreparer {
    pub fn new(resolver: SharedResolver) -> Self {
        Self {
            resolver,
            session: None,
            accepted: HashSet::new(),
            capacity: MAX_TRACKED_ROWS,
            capacity_reached: false,
        }
    }

    pub fn prepare(&mut self, connection_id: u64, message: &ServerMessage) -> PreparedGeoMessage {
        let mut prepared = PreparedGeoMessage {
            message: None,
            resolutions: HashMap::new(),
            reset: false,
            capacity_reached: self.capacity_reached,
            activity_codes: Vec::new(),
        };
        // Connection generations increase monotonically. A stale delivery must
        // not rewind the accepted-ID shadow after a newer session's reset.
        if self.session.is_some_and(|session| connection_id < session) {
            return prepared;
        }
        if self.session != Some(connection_id) {
            self.session = Some(connection_id);
            self.accepted.clear();
            self.capacity_reached = false;
            prepared.reset = true;
        }
        match message {
            ServerMessage::InsertConnectionRows { rows }
            | ServerMessage::UpdateConnectionRows { rows } => {
                let insert = matches!(message, ServerMessage::InsertConnectionRows { .. });
                let mut accepted_rows = BTreeMap::new();
                for row in rows {
                    if !self.accepted.contains(&row.id) {
                        if !insert {
                            continue;
                        }
                        if self.accepted.len() >= self.capacity {
                            self.capacity_reached = true;
                            continue;
                        }
                        self.accepted.insert(row.id.clone());
                    }
                    let resolved = self.resolver.resolve(&row.dst_ip);
                    // Final state per ID is sufficient for aggregation. This
                    // bounds duplicates as well as oversized incoming batches.
                    accepted_rows.insert(row.id.clone(), (row.clone(), resolved));
                }
                let mut rows = Vec::with_capacity(accepted_rows.len());
                let mut activity = BTreeSet::new();
                for (id, (row, resolved)) in accepted_rows {
                    if let Bucket::Country(code) = &resolved.bucket {
                        activity.insert(code.clone());
                    }
                    prepared.resolutions.insert(id, resolved);
                    rows.push(row);
                }
                prepared.activity_codes = activity.into_iter().collect();
                prepared.message = Some(if insert {
                    ServerMessage::InsertConnectionRows { rows }
                } else {
                    ServerMessage::UpdateConnectionRows { rows }
                });
            }
            ServerMessage::RemoveConnectionRows { ids } => {
                let mut removed = BTreeSet::new();
                for id in ids {
                    if self.accepted.remove(id) {
                        removed.insert(id.clone());
                    }
                }
                prepared.message = Some(ServerMessage::RemoveConnectionRows {
                    ids: removed.into_iter().collect(),
                });
            }
            ServerMessage::ClearConnectionRows => {
                self.accepted.clear();
                self.capacity_reached = false;
                prepared.message = Some(ServerMessage::ClearConnectionRows);
            }
            _ => {}
        }
        prepared.capacity_reached = self.capacity_reached;
        prepared
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::resolver::{CountryInfo, CountryLookup};
    use crate::geo::store::GeoStore;
    use snitchwatch_bridge::ws_messages::ConnectionRow;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn row(id: &str, ip: &str) -> ConnectionRow {
        serde_json::from_value(serde_json::json!({"id":id,"process":"app","processPath":null,"dstHost":"observed.example","dstIp":ip,"dstPort":443,"protocol":"tcp","direction":"outgoing","action":"allow","bytesSent":0,"bytesReceived":0,"startedAtMs":0})).unwrap()
    }
    struct Counting(Arc<AtomicUsize>);
    impl CountryLookup for Counting {
        fn lookup(&self, _: std::net::IpAddr) -> Option<CountryInfo> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Some(CountryInfo {
                code: "US".into(),
                name: "United States".into(),
            })
        }
    }
    #[test]
    fn tiny_cache_and_small_capacity_keep_late_updates_removals_and_session_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = SharedResolver::with_cache_limit(Some(Arc::new(Counting(calls.clone()))), 1);
        let mut preparer = GeoMessagePreparer::new(resolver.clone());
        preparer.capacity = 2;
        let mut store = GeoStore::with_resolver(resolver.clone());
        let first = preparer.prepare(
            1,
            &ServerMessage::InsertConnectionRows {
                rows: vec![
                    row("a", "8.8.8.8"),
                    row("b", "1.1.1.1"),
                    row("overflow", "9.9.9.9"),
                    row("a", "8.8.4.4"),
                ],
            },
        );
        assert_eq!(first.resolutions.len(), 2);
        assert!(first.capacity_reached);
        let before = calls.load(Ordering::Relaxed);
        assert!(store.apply_prepared(&first));
        assert_eq!(calls.load(Ordering::Relaxed), before);
        assert_eq!(store.hierarchy().observed_connections, 2);
        assert!(
            store.hierarchy().countries["country:US"].cities["country:US:unknown-city"]
                .ips
                .contains_key("ip:8.8.4.4")
        );
        let next = preparer.prepare(
            1,
            &ServerMessage::UpdateConnectionRows {
                rows: vec![row("missing", "9.9.9.9"), row("a", "8.8.8.8")],
            },
        );
        let before = calls.load(Ordering::Relaxed);
        store.apply_prepared(&next);
        assert_eq!(calls.load(Ordering::Relaxed), before);
        let remove = preparer.prepare(
            1,
            &ServerMessage::RemoveConnectionRows {
                ids: vec!["b".into(), "b".into(), "overflow".into()],
            },
        );
        store.apply_prepared(&remove);
        let reinsert = preparer.prepare(
            1,
            &ServerMessage::InsertConnectionRows {
                rows: vec![row("next", "9.9.9.9")],
            },
        );
        store.apply_prepared(&reinsert);
        assert_eq!(store.hierarchy().observed_connections, 2);
        assert!(store.capacity_reached());
        let clear = preparer.prepare(1, &ServerMessage::ClearConnectionRows);
        store.apply_prepared(&clear);
        assert_eq!(store.hierarchy().observed_connections, 0);
        assert!(!store.capacity_reached());
        let reset = preparer.prepare(
            2,
            &ServerMessage::InsertConnectionRows {
                rows: vec![row("new", "1.1.1.1")],
            },
        );
        assert!(reset.reset);
        store.apply_prepared(&reset);
        let stale = preparer.prepare(1, &ServerMessage::ClearConnectionRows);
        assert!(!stale.reset && stale.message.is_none());
        store.apply_prepared(&stale);
        assert_eq!(store.hierarchy().observed_connections, 1);
        let mut qualified = preparer.prepare(
            2,
            &ServerMessage::UpdateConnectionRows {
                rows: vec![row("new", "8.8.8.8")],
            },
        );
        qualified.qualify_ids(2);
        assert!(qualified.resolutions.contains_key("2:new"));
    }
}
