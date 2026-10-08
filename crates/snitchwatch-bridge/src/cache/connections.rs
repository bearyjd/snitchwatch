//! Rolling connection-row buffer with pending-prompt machinery.
//!
//! Invariants:
//!   - Pending rows are never evicted by retention policy
//!   - Eviction is by insertion order, oldest non-pending first
//!   - Each pending row owns a oneshot::Sender that resolves to the verdict;
//!     this is what the gRPC client task awaits before responding to AskRule

use crate::client_presence::Admission;
use crate::filter_pause::FilterPause;
use crate::tray_state::{TrayState, TrayStatePublisher};
use crate::ws_messages::{ConnectionRow, VerdictDuration, VerdictScope};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};

#[derive(Debug)]
pub struct PendingHandle {
    pub row_id: String,
    pub sender: oneshot::Sender<VerdictResolution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

/// What the `AskRule` gRPC handler needs to build the reply `Rule`: the
/// allow/deny decision, how long it should live, and how broadly it should
/// scope the resulting `Operator` (see `ws_messages::VerdictScope`). Carried
/// over the same oneshot channel `Verdict` used to travel alone on — see
/// `translator::verdict::verdict_to_rule`, the sole consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictResolution {
    pub verdict: Verdict,
    pub duration: VerdictDuration,
    pub scope: VerdictScope,
}

struct PendingEntry {
    sender: oneshot::Sender<VerdictResolution>,
    admission: Option<Admission>,
    cancellations: Option<broadcast::Sender<crate::ws_messages::ServerMessage>>,
}

pub struct ConnectionCache {
    rows: Vec<ConnectionRow>,
    pending: HashMap<String, PendingEntry>,
    capacity: usize,
    tray: Option<Arc<TrayStatePublisher>>,
    filter_pause: Option<Arc<FilterPause>>,
    /// Set by the daemon watchdog (issue #58): every republish keeps the
    /// tray on `DaemonDown` while it is.
    daemon_down: bool,
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
            filter_pause: None,
            daemon_down: false,
        }
    }

    pub fn with_tray_publisher(capacity: usize, tray: Arc<TrayStatePublisher>) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
            pending: HashMap::new(),
            capacity,
            tray: Some(tray),
            filter_pause: None,
            daemon_down: false,
        }
    }

    /// Every tray republish shows `FilterOff` while this pause is active
    /// (issue #47), so no reset path puts the tray back to Idle/Pending
    /// mid-pause.
    pub fn with_filter_pause(mut self, filter_pause: Arc<FilterPause>) -> Self {
        self.filter_pause = Some(filter_pause);
        self
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// What the tray should show now: [`TrayState::derive`] over the daemon
    /// state, the pause and the pending prompts.
    pub fn tray_state(&self) -> TrayState {
        let paused = self
            .filter_pause
            .as_ref()
            .is_some_and(|pause| pause.is_active_now());
        TrayState::derive(self.daemon_down, paused, self.pending_count())
    }

    /// Record the watchdog's daemon state (issue #58). The watchdog publishes
    /// the resulting [`Self::tray_state`] itself.
    pub fn set_daemon_down(&mut self, down: bool) {
        self.daemon_down = down;
    }

    fn republish_pending_count(&self) {
        if let Some(tray) = &self.tray {
            tray.set(self.tray_state());
        }
    }

    /// Publish [`Self::tray_state`], regardless of what the tray is currently
    /// showing. Used where the tray must come back to what is true now rather
    /// than to `Idle`: a `RecentBlock` timer's revert, and a pause starting,
    /// ending or expiring (`announce_pause_state` in the bridge CLI). The
    /// daemon watchdog does not use it; it publishes [`Self::tray_state`]
    /// itself while it holds the cache lock.
    pub fn resync_tray_state(&self) {
        self.republish_pending_count();
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
    pub fn insert_pending(&mut self, row: ConnectionRow) -> oneshot::Receiver<VerdictResolution> {
        self.insert_pending_inner(row, None, None)
    }

    pub(crate) fn insert_admitted(
        &mut self,
        row: ConnectionRow,
        admission: Admission,
        cancellations: broadcast::Sender<crate::ws_messages::ServerMessage>,
    ) -> Option<oneshot::Receiver<VerdictResolution>> {
        admission
            .clone()
            .while_current(|| self.insert_pending_inner(row, Some(admission), Some(cancellations)))
    }

    fn insert_pending_inner(
        &mut self,
        row: ConnectionRow,
        admission: Option<Admission>,
        cancellations: Option<broadcast::Sender<crate::ws_messages::ServerMessage>>,
    ) -> oneshot::Receiver<VerdictResolution> {
        debug_assert!(row.action.is_none(), "pending rows must have action=None");
        let id = row.id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.insert(
            id,
            PendingEntry {
                sender: tx,
                admission,
                cancellations,
            },
        );
        self.rows.push(row);
        self.evict_if_needed();
        self.republish_pending_count();
        rx
    }

    /// Resolve a pending row with a verdict and the duration its resulting
    /// rule should live for. Returns Err if the row isn't pending.
    pub fn resolve(
        &mut self,
        row_id: &str,
        verdict: Verdict,
        duration: VerdictDuration,
        scope: VerdictScope,
    ) -> Result<(), CacheError> {
        let entry = self
            .pending
            .remove(row_id)
            .ok_or_else(|| CacheError::NotPending(row_id.to_string()))?;
        let cancellations = entry.cancellations;
        let resolution = VerdictResolution {
            verdict,
            duration,
            scope,
        };
        let settle = || {
            if entry.sender.send(resolution).is_err() {
                return false;
            }
            true
        };
        let delivered = match entry.admission {
            Some(admission) => admission.while_current(settle).unwrap_or(false),
            None => settle(),
        };
        if !delivered {
            self.rows.retain(|row| row.id != row_id);
            Self::publish_cancellation(cancellations, row_id);
            self.republish_pending_count();
            return Err(CacheError::NotPending(row_id.to_string()));
        }

        // Update the row's action so future re-renders show it as decided,
        // and record which rule now governs this connection: the same
        // synthetic once-off rule name `verdict_to_rule` hands back to
        // opensnitchd as the `AskRule` reply (see `rule_name_for`'s doc
        // comment for why this is the single source of truth for that name).
        let result = if let Some(row) = self.rows.iter_mut().find(|r| r.id == row_id) {
            row.action = Some(match verdict {
                Verdict::Allow => "allow".to_string(),
                Verdict::Deny => "deny".to_string(),
            });
            row.matched_rule = Some(crate::translator::verdict::rule_name_for(
                verdict,
                &row.dst_host,
                row.dst_port,
                row.process_path.as_deref().unwrap_or(""),
            ));
            row.answer_deadline_ms = None;
            Ok(())
        } else {
            Err(CacheError::NotFound(row_id.to_string()))
        };
        self.republish_pending_count();
        result
    }

    /// Cancel only unresolved prompts; a verdict that already won is preserved.
    pub fn cancel_pending(&mut self, row_id: &str) -> bool {
        let Some(entry) = self.pending.remove(row_id) else {
            return false;
        };
        Self::publish_cancellation(entry.cancellations, row_id);
        self.rows.retain(|row| row.id != row_id);
        self.republish_pending_count();
        true
    }

    /// Put off an unresolved prompt without a verdict (prompt-slot plan
    /// Part C). Like [`Self::cancel_pending`], a verdict that already won is
    /// preserved, but the row stays listed: deferred, with `action` (the
    /// daemon's default action, when known) and `why`. Dropping the verdict
    /// sender makes the waiting `AskRule` give the daemon no answer.
    pub(crate) fn defer_pending(
        &mut self,
        row_id: &str,
        action: Option<&str>,
        why: Option<crate::ws_messages::AutoAnswer>,
    ) -> Option<ConnectionRow> {
        self.pending.remove(row_id)?;
        self.republish_pending_count();
        let row = self.rows.iter_mut().find(|row| row.id == row_id)?;
        *row = ConnectionRow {
            action: action.map(str::to_owned),
            auto_answer: why,
            answer_deadline_ms: None,
            deferred: true,
            ..row.clone()
        };
        Some(row.clone())
    }

    /// Mark a row resolved by "Decide later" as deferred and return it.
    pub(crate) fn mark_deferred(&mut self, row_id: &str) -> Option<ConnectionRow> {
        let row = self.rows.iter_mut().find(|row| row.id == row_id)?;
        row.deferred = true;
        Some(row.clone())
    }

    fn publish_cancellation(
        broadcast: Option<broadcast::Sender<crate::ws_messages::ServerMessage>>,
        row_id: &str,
    ) {
        if let Some(broadcast) = broadcast {
            let _ = broadcast.send(crate::ws_messages::ServerMessage::RemoveConnectionRows {
                ids: vec![row_id.to_owned()],
            });
        }
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }

    /// Pending rows whose `AskRule` admission passes `keep`. Rows inserted
    /// without an admission never do.
    pub(crate) fn pending_admitted_where(&self, keep: impl Fn(&Admission) -> bool) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, entry)| entry.admission.as_ref().is_some_and(&keep))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Record why the bridge answered `row_id` itself (issue #78) and
    /// return the updated row.
    pub(crate) fn label_auto_answer(
        &mut self,
        row_id: &str,
        why: crate::ws_messages::AutoAnswer,
    ) -> Option<ConnectionRow> {
        let row = self.rows.iter_mut().find(|row| row.id == row_id)?;
        row.auto_answer = Some(why);
        Some(row.clone())
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
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
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
        c.resolve(
            "p1",
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
        let received = rx.await.unwrap();
        assert_eq!(received.verdict, Verdict::Allow);
        assert_eq!(received.duration, VerdictDuration::Once);
        assert_eq!(c.rows()[0].action.as_deref(), Some("allow"));
    }

    #[tokio::test]
    async fn resolve_carries_the_requested_duration() {
        let mut c = ConnectionCache::new(10);
        let rx = c.insert_pending(pending_row("p1"));
        c.resolve(
            "p1",
            Verdict::Allow,
            VerdictDuration::Always,
            VerdictScope::ThisHost,
        )
        .unwrap();
        let received = rx.await.unwrap();
        assert_eq!(received.duration, VerdictDuration::Always);
    }

    #[tokio::test]
    async fn resolve_populates_matched_rule_with_the_synthetic_rule_name() {
        let mut c = ConnectionCache::new(10);
        let _rx = c.insert_pending(pending_row("p1"));
        c.resolve(
            "p1",
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
        // Rule names now always carry an appended raw-input digest (issue
        // #14 security review round 2, MEDIUM-2), so compute the expected
        // value via the same single-source-of-truth function rather than
        // hardcoding the hash.
        let expected = crate::translator::verdict::rule_name_for(Verdict::Deny, "h", 443, "");
        assert_eq!(c.rows()[0].matched_rule.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn resolve_matched_rule_names_the_requesting_program() {
        // Issue #44: app-bound rules carry the program in their name, so the
        // "Show rule" jump must compute the same process-qualified name.
        let mut c = ConnectionCache::new(10);
        let mut row = pending_row("p1");
        row.process_path = Some("/usr/bin/curl".to_string());
        let _rx = c.insert_pending(row);
        c.resolve(
            "p1",
            Verdict::Allow,
            VerdictDuration::Always,
            VerdictScope::ThisHost,
        )
        .unwrap();
        let expected =
            crate::translator::verdict::rule_name_for(Verdict::Allow, "h", 443, "/usr/bin/curl");
        assert_eq!(c.rows()[0].matched_rule.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn disconnect_and_verdict_race_settles_exactly_once() {
        for _ in 0..64 {
            let presence = crate::client_presence::ClientPresence::default();
            let lease = presence.authenticated_session();
            let (tx, mut messages) = broadcast::channel(8);
            let mut cache = ConnectionCache::new(8);
            let receiver = cache
                .insert_admitted(pending_row("race"), presence.admit().unwrap(), tx)
                .unwrap();
            let cache = Arc::new(std::sync::Mutex::new(cache));
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let loss_barrier = barrier.clone();
            let loss = std::thread::spawn(move || {
                loss_barrier.wait();
                drop(lease);
            });
            barrier.wait();
            let outcome = cache.lock().unwrap().resolve(
                "race",
                Verdict::Allow,
                VerdictDuration::Always,
                VerdictScope::ThisHost,
            );
            loss.join().unwrap();
            let mut cache = cache.lock().unwrap();
            assert_eq!(cache.pending_count(), 0);
            assert!(!cache.cancel_pending("race"));
            assert!(cache
                .resolve(
                    "race",
                    Verdict::Deny,
                    VerdictDuration::Always,
                    VerdictScope::AnyHost
                )
                .is_err());
            if outcome.is_ok() {
                assert_eq!(cache.rows()[0].action.as_deref(), Some("allow"));
                assert_eq!(receiver.blocking_recv().unwrap().verdict, Verdict::Allow);
                assert!(messages.try_recv().is_err());
            } else {
                assert!(cache.is_empty());
                assert!(receiver.blocking_recv().is_err());
                assert!(matches!(messages.try_recv().unwrap(),
                    crate::ws_messages::ServerMessage::RemoveConnectionRows { ids } if ids == ["race"]));
                assert!(messages.try_recv().is_err());
            }
        }
    }

    #[test]
    fn resolve_unknown_row_errors() {
        let mut c = ConnectionCache::new(10);
        let err = c
            .resolve(
                "nope",
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .unwrap_err();
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
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
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

        cache
            .resolve(
                "1",
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .unwrap();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Idle);
    }

    fn paused_tray_cache() -> (
        ConnectionCache,
        Arc<FilterPause>,
        tokio::sync::watch::Receiver<TrayState>,
    ) {
        let tray = Arc::new(TrayStatePublisher::new());
        let rx = tray.subscribe();
        let pause = Arc::new(FilterPause::new());
        let cache = ConnectionCache::with_tray_publisher(64, tray).with_filter_pause(pause.clone());
        (cache, pause, rx)
    }

    /// Issue #58: while the daemon is down no resync shows anything else: not
    /// a prompt arriving, resolved or cancelled, a recent-block revert, or a
    /// pause starting or ending.
    #[test]
    fn every_resync_keeps_daemon_down_while_the_daemon_is_down() {
        let (mut cache, pause, rx) = paused_tray_cache();
        cache.set_daemon_down(true);
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);

        let _first = cache.insert_pending(pending_row("1"));
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a prompt arriving");
        let _second = cache.insert_pending(pending_row("2"));
        cache
            .resolve(
                "1",
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .unwrap();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a prompt resolved");
        assert!(cache.cancel_pending("2"));
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a prompt cancelled");
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a recent-block revert");
        pause.pause(std::time::Duration::from_secs(300), 0).unwrap();
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a pause starting");
        pause.resume();
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown, "a pause ending");
    }

    #[test]
    fn the_daemon_coming_back_shows_what_the_other_inputs_call_for() {
        let (mut cache, pause, rx) = paused_tray_cache();
        cache.set_daemon_down(true);
        let _prompt = cache.insert_pending(pending_row("1"));
        cache.set_daemon_down(false);
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::Pending(1));

        pause.pause(std::time::Duration::from_secs(300), 0).unwrap();
        cache.set_daemon_down(true);
        cache.resync_tray_state();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);
        cache.set_daemon_down(false);
        cache.resync_tray_state();
        assert_eq!(
            *rx.borrow(),
            TrayState::FilterOff,
            "the pause outranks the prompt"
        );
    }
}
