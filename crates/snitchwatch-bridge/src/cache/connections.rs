//! Rolling connection-row buffer with pending-prompt machinery.
//!
//! Invariants:
//!   - Pending rows are never evicted by retention policy
//!   - Eviction is by insertion order, oldest non-pending first
//!   - Each pending row owns a oneshot::Sender that resolves to the verdict;
//!     this is what the gRPC client task awaits before responding to AskRule

use crate::tray_state::{TrayState, TrayStatePublisher};
use crate::ws_messages::ConnectionRow;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;

#[derive(Debug)]
pub struct PendingHandle {
    pub row_id: String,
    pub sender: oneshot::Sender<Verdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

pub struct ConnectionCache {
    rows: Vec<ConnectionRow>,
    pending: HashMap<String, oneshot::Sender<Verdict>>,
    capacity: usize,
    tray: Option<Arc<TrayStatePublisher>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("row not found: {0}")]
    NotFound(String),
    #[error("row {0} is not pending")]
    NotPending(String),
}

impl ConnectionCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
            pending: HashMap::new(),
            capacity,
            tray: None,
        }
    }

    pub fn with_tray_publisher(capacity: usize, tray: Arc<TrayStatePublisher>) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
            pending: HashMap::new(),
            capacity,
            tray: Some(tray),
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn republish_pending_count(&self) {
        if let Some(tray) = &self.tray {
            let n = self.pending_count();
            tray.set(if n == 0 {
                TrayState::Idle
            } else {
                TrayState::Pending(n)
            });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Insert a fully-decided row (e.g. from a Ping stats delta).
    pub fn insert_decided(&mut self, row: ConnectionRow) {
        debug_assert!(row.action.is_some(), "use insert_pending for action=None");
        self.rows.push(row);
        self.evict_if_needed();
    }

    /// Insert a pending row and return the receiver side of its verdict
    /// channel. The gRPC client task awaits this receiver before responding
    /// to the AskRule call.
    pub fn insert_pending(&mut self, row: ConnectionRow) -> oneshot::Receiver<Verdict> {
        debug_assert!(row.action.is_none(), "pending rows must have action=None");
        let id = row.id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        self.rows.push(row);
        self.evict_if_needed();
        self.republish_pending_count();
        rx
    }

    /// Resolve a pending row with a verdict. Returns Err if the row isn't
    /// pending.
    pub fn resolve(&mut self, row_id: &str, verdict: Verdict) -> Result<(), CacheError> {
        let sender = self
            .pending
            .remove(row_id)
            .ok_or_else(|| CacheError::NotPending(row_id.to_string()))?;
        // It's fine if the receiver was dropped (e.g. gRPC stream broke).
        let _ = sender.send(verdict);

        // Update the row's action so future re-renders show it as decided.
        let result = if let Some(row) = self.rows.iter_mut().find(|r| r.id == row_id) {
            row.action = Some(match verdict {
                Verdict::Allow => "allow".to_string(),
                Verdict::Deny => "deny".to_string(),
            });
            Ok(())
        } else {
            Err(CacheError::NotFound(row_id.to_string()))
        };
        self.republish_pending_count();
        result
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }

    pub fn rows(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// Evict oldest non-pending rows until we're at or under capacity.
    fn evict_if_needed(&mut self) {
        while self.rows.len() > self.capacity {
            // Find the first non-pending row (FIFO eviction).
            let idx = self
                .rows
                .iter()
                .position(|r| !self.pending.contains_key(&r.id));
            match idx {
                Some(i) => {
                    self.rows.remove(i);
                }
                None => {
                    // All rows are pending — we cannot evict, capacity is
                    // effectively the pending count. Log a warning so the
                    // operator notices.
                    tracing::warn!(
                        capacity = self.capacity,
                        len = self.rows.len(),
                        "all rows are pending; cache exceeds capacity"
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided_row(id: &str, action: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "p".to_string(),
            process_path: None,
            dst_host: "h".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: Some(action.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    fn pending_row(id: &str) -> ConnectionRow {
        let mut r = decided_row(id, "allow");
        r.action = None;
        r
    }

    #[test]
    fn insert_decided_grows_the_cache() {
        let mut c = ConnectionCache::new(10);
        c.insert_decided(decided_row("a", "allow"));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn eviction_removes_oldest_non_pending_first() {
        let mut c = ConnectionCache::new(2);
        c.insert_decided(decided_row("a", "allow"));
        c.insert_decided(decided_row("b", "allow"));
        c.insert_decided(decided_row("c", "allow"));
        let ids: Vec<&str> = c.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
    }

    #[test]
    fn pending_rows_are_not_evicted() {
        let mut c = ConnectionCache::new(2);
        c.insert_decided(decided_row("a", "allow"));
        let _rx = c.insert_pending(pending_row("p1"));
        c.insert_decided(decided_row("c", "allow"));
        let ids: Vec<&str> = c.rows().iter().map(|r| r.id.as_str()).collect();
        // "a" was evicted (oldest non-pending); "p1" stayed.
        assert_eq!(ids, vec!["p1", "c"]);
    }

    #[tokio::test]
    async fn resolve_fires_oneshot_and_updates_row() {
        let mut c = ConnectionCache::new(10);
        let rx = c.insert_pending(pending_row("p1"));
        c.resolve("p1", Verdict::Allow).unwrap();
        let received = rx.await.unwrap();
        assert_eq!(received, Verdict::Allow);
        assert_eq!(c.rows()[0].action.as_deref(), Some("allow"));
    }

    #[test]
    fn resolve_unknown_row_errors() {
        let mut c = ConnectionCache::new(10);
        let err = c.resolve("nope", Verdict::Allow).unwrap_err();
        assert!(matches!(err, CacheError::NotPending(_)));
    }

    #[test]
    fn cache_can_exceed_capacity_when_all_rows_pending() {
        let mut c = ConnectionCache::new(2);
        let _r1 = c.insert_pending(pending_row("p1"));
        let _r2 = c.insert_pending(pending_row("p2"));
        let _r3 = c.insert_pending(pending_row("p3"));
        // No eviction possible — pending rows are sacred.
        assert_eq!(c.len(), 3);
    }
}

#[cfg(test)]
mod tray_state_tests {
    use super::*;
    use crate::tray_state::{TrayState, TrayStatePublisher};
    use std::sync::Arc;

    fn pending_row(id: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "firefox".to_string(),
            process_path: None,
            dst_host: "github.com".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    #[tokio::test]
    async fn inserting_pending_row_publishes_count() {
        let tray = Arc::new(TrayStatePublisher::new());
        let mut rx = tray.subscribe();
        let mut cache = ConnectionCache::with_tray_publisher(64, tray.clone());

        let _verdict_rx = cache.insert_pending(pending_row("1"));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(1));

        let _verdict_rx2 = cache.insert_pending(pending_row("2"));
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(2));
    }

    #[tokio::test]
    async fn resolving_last_pending_returns_to_idle() {
        let tray = Arc::new(TrayStatePublisher::new());
        let mut rx = tray.subscribe();
        let mut cache = ConnectionCache::with_tray_publisher(64, tray.clone());

        let _verdict_rx = cache.insert_pending(pending_row("1"));
        rx.changed().await.unwrap();

        cache.resolve("1", Verdict::Allow).unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Idle);
    }
}
