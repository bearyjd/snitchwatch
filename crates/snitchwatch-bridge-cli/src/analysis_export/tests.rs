use super::*;
use snitchwatch_bridge::ws_messages::{ReplyTo, ServerMessage};
use std::{
    sync::{atomic::AtomicUsize, Condvar},
    time::Duration,
};
use tokio::sync::{mpsc, Notify};

struct Peer {
    session: ExportSession,
    reply: ReplyTo,
    replies: mpsc::Receiver<ServerMessage>,
}
impl Peer {
    fn new() -> Self {
        let (tx, replies) = mpsc::channel(32);
        Self {
            session: ExportSession::new(),
            reply: ReplyTo::new(tx),
            replies,
        }
    }
    fn request(&self, id: &str) -> ClientMessage {
        ClientMessage::RequestCacheSummaryExport {
            request_id: id.into(),
            schema_version: 1,
            filters: ExportFilters::default(),
            reply: Some(self.reply.clone()),
            session: Some(self.session.clone()),
        }
    }
    fn cancel(&self, id: &str) -> ClientMessage {
        ClientMessage::CancelCacheSummaryExport {
            request_id: id.into(),
            schema_version: 1,
            reply: Some(self.reply.clone()),
            session: Some(self.session.clone()),
        }
    }
    fn reason(&mut self) -> Option<ExportError> {
        match self.replies.try_recv().ok()? {
            ServerMessage::CacheSummaryExportRefused { reason, .. } => Some(reason),
            _ => panic!("unexpected private status"),
        }
    }
}
fn cache() -> Arc<tokio::sync::Mutex<ConnectionCache>> {
    Arc::new(tokio::sync::Mutex::new(ConnectionCache::new(20)))
}
async fn finish(service: &AnalysisExports) {
    let jobs = std::mem::take(&mut service.inner.state.lock().unwrap().jobs);
    for job in jobs {
        job.await.unwrap();
    }
}
#[derive(Default)]
struct Hooks {
    hold_reap: AtomicBool,
    reaped: AtomicUsize,
    reap_gate: Arc<DeadlineGate>,
    force_deadline: AtomicBool,
    deadline_gate: Arc<DeadlineGate>,
    deadlines: Mutex<Vec<std::time::Instant>>,
    stall_after_publish: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    entered: AtomicUsize,
    notify: Notify,
    gate: Mutex<bool>,
    changed: Condvar,
    stall: AtomicBool,
    attempts: AtomicUsize,
    collision: AtomicBool,
    fail_rng: AtomicBool,
    cancel_retry: AtomicBool,
    unlocked: AtomicBool,
    cache: Option<Arc<tokio::sync::Mutex<ConnectionCache>>>,
}
impl Hooks {
    fn release(&self) {
        *self.gate.lock().unwrap() = true;
        self.changed.notify_all();
    }
    async fn wait_entered(&self, count: usize) {
        let reached = async {
            loop {
                let notified = self.notify.notified();
                if self.entered.load(Ordering::Acquire) >= count {
                    break;
                }
                notified.await;
            }
        };
        assert!(
            tokio::time::timeout(Duration::from_secs(2), reached)
                .await
                .is_ok(),
            "worker did not enter"
        );
    }
}
struct Release(Arc<Hooks>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
        self.0.reap_gate.fire();
        self.0.deadline_gate.fire();
    }
}
#[derive(Default)]
struct DeadlineGate {
    fired: AtomicBool,
    changed: Notify,
}
impl DeadlineGate {
    fn fire(&self) {
        self.fired.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.fired.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }
}
impl WorkerHooks for Hooks {
    fn before_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.reaped.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
        if self.hold_reap.load(Ordering::Acquire) {
            let gate = self.reap_gate.clone();
            Box::pin(async move { gate.wait().await })
        } else {
            Box::pin(std::future::ready(()))
        }
    }
    fn wait_for_deadline(
        &self,
        deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.deadlines.lock().unwrap().push(deadline);
        if !self.force_deadline.load(Ordering::Acquire) {
            return Production.wait_for_deadline(deadline);
        }
        let gate = self.deadline_gate.clone();
        Box::pin(async move { gate.wait().await })
    }
    fn published(&self) {
        if self.stall_after_publish.load(Ordering::Acquire) {
            let mut gate = self.gate.lock().unwrap();
            while !*gate {
                gate = self.changed.wait(gate).unwrap();
            }
        }
    }
    fn entered(&self, _: &ExportControl) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak.fetch_max(active, Ordering::AcqRel);
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_one();
        if self.stall.load(Ordering::Acquire) {
            let mut gate = self.gate.lock().unwrap();
            while !*gate {
                gate = self.changed.wait(gate).unwrap();
            }
        }
    }
    fn exited(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
    fn snapshot_ready(&self) {
        if let Some(cache) = &self.cache {
            self.unlocked
                .store(cache.try_lock().is_ok(), Ordering::Release);
        }
    }
    fn attempt(
        &self,
        snapshot: &Snapshot,
        filters: &ExportFilters,
        control: &ExportControl,
    ) -> Result<ExportDocument, ExportError> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        if self.fail_rng.load(Ordering::Acquire) {
            return Err(ExportError::Internal);
        }
        let key = core::fresh_key()?;
        if self.cancel_retry.load(Ordering::Acquire) {
            control.cancelled.store(true, Ordering::Release);
        }
        if self.collision.load(Ordering::Acquire) {
            Err(ExportError::Collision)
        } else {
            core::build(snapshot, filters, &key, control)
        }
    }
}
#[test]
fn weak_session_metadata_does_not_retain_or_reopen_a_connection() {
    let session = ExportSession::new();
    let weak = session.downgrade();
    assert!(weak.upgrade().is_some());
    session.close();
    assert!(weak.upgrade().unwrap().is_closed());
    drop(session);
    assert!(weak.upgrade().is_none());
}
#[tokio::test]
async fn key_collision_retry_is_bounded_and_no_other_error_retries() {
    for (collision, rng, cancel, expected, calls) in [
        (true, false, false, ExportError::Collision, 3),
        (false, true, false, ExportError::Internal, 1),
        (true, false, true, ExportError::Cancelled, 1),
    ] {
        let hooks = Arc::new(Hooks::default());
        hooks.collision.store(collision, Ordering::Release);
        hooks.fail_rng.store(rng, Ordering::Release);
        hooks.cancel_retry.store(cancel, Ordering::Release);
        let service = AnalysisExports::with_hooks(cache(), hooks.clone());
        let mut peer = Peer::new();
        service.try_route(peer.request("retry"));
        finish(&service).await;
        assert_eq!(hooks.attempts.load(Ordering::Acquire), calls);
        assert_eq!(peer.reason(), Some(expected));
    }
}
#[tokio::test]
async fn capture_unlocks_before_build_and_registry_deduplicates_and_prunes() {
    let cache = cache();
    let hooks = Arc::new(Hooks {
        cache: Some(cache.clone()),
        ..Hooks::default()
    });
    let service = AnalysisExports::with_hooks(cache, hooks.clone());
    let peer = Peer::new();
    for _ in 0..10 {
        service.try_route(peer.request("same-session"));
        finish(&service).await;
        let reply = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply())
            .await
            .unwrap()
            .unwrap();
        drop(reply);
        assert_eq!(service.inner.state.lock().unwrap().sessions.len(), 1);
    }
    assert!(hooks.unlocked.load(Ordering::Acquire));
    drop(peer);
    let next = Peer::new();
    service.try_route(next.request("next"));
    finish(&service).await;
    assert_eq!(service.inner.state.lock().unwrap().sessions.len(), 1);
    next.session.close();
    let another = Peer::new();
    service.try_route(another.request("another"));
    finish(&service).await;
    assert_eq!(service.inner.state.lock().unwrap().sessions.len(), 1);
}
#[tokio::test]
async fn absent_private_context_never_exports_or_falls_back() {
    let hooks = Arc::new(Hooks::default());
    let service = AnalysisExports::with_hooks(cache(), hooks.clone());
    let mut peer = Peer::new();
    let mut request = peer.request("absent");
    if let ClientMessage::RequestCacheSummaryExport { session, .. } = &mut request {
        *session = None;
    }
    assert!(service.try_route(request).is_none());
    let mut request = peer.request("absent");
    if let ClientMessage::RequestCacheSummaryExport { reply, .. } = &mut request {
        *reply = None;
    }
    assert!(service.try_route(request).is_none());
    assert!(peer.replies.try_recv().is_err());
    assert_eq!(hooks.entered.load(Ordering::Acquire), 0);
}
#[test]
fn real_service_hookup_advertises_the_capability() {
    assert!(snitchwatch_bridge::bridge_capabilities::advertised()
        .iter()
        .any(|cap| cap == snitchwatch_bridge::bridge_capabilities::AI_CACHE_SUMMARY_EXPORT_V1));
}

#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
#[path = "service_tests.rs"]
mod service_tests;
