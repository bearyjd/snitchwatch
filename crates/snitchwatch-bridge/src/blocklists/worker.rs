//! The single blocklist worker (issue #45 PR A).
//!
//! Every subscribe, unsubscribe and scheduled refresh becomes a
//! [`BlocklistJob`] on one bounded queue, run in order by one task:
//! - the inbound pump only enqueues ([`BlocklistWorker::try_route`] never
//!   awaits), so a fetch — up to `FETCH_TIMEOUT` — never stalls verdicts;
//! - at most one fetch, and so at most one `MAX_BODY_BYTES` body, is in
//!   flight at a time.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::blocklists::{spawn_event_pump, BlocklistsManager};
use crate::translator::upstream::handle_blocklist_action;
use crate::ws_messages::{ClientMessage, ServerMessage};

/// Queued jobs beyond this are refused (a subscribe is shown as failed).
pub const JOB_QUEUE_CAPACITY: usize = 64;
/// How often the refresh loop asks the worker to refresh due subscriptions.
/// Each subscription is only re-fetched once its own interval (24 h) elapses.
pub const DEFAULT_REFRESH_TICK: Duration = Duration::from_secs(15 * 60);

const QUEUE_FULL_REASON: &str = "too many blocklist requests are waiting; try again later";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlocklistJob {
    Subscribe { url: String },
    Unsubscribe { id: String },
    RefreshDue,
}

/// Cheap, cloneable handle to the worker's queue.
#[derive(Clone)]
pub struct BlocklistWorker {
    tx: mpsc::Sender<BlocklistJob>,
    mgr: Arc<BlocklistsManager>,
}

impl BlocklistWorker {
    /// Spawn the worker task. It ends once every handle is dropped (or when
    /// aborted).
    pub fn spawn(mgr: Arc<BlocklistsManager>) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel(JOB_QUEUE_CAPACITY);
        let worker_mgr = mgr.clone();
        let handle = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                run_job(&worker_mgr, job).await;
            }
        });
        (Self { tx, mgr }, handle)
    }

    /// Queue `job` without waiting. Returns false (and logs) if the queue is
    /// full or the worker is gone.
    pub fn enqueue(&self, job: BlocklistJob) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(job)) => {
                warn!(?job, "blocklist worker queue full; job dropped");
                if let BlocklistJob::Subscribe { url } = job {
                    self.mgr.reject_subscription(&url, QUEUE_FULL_REASON);
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(job)) => {
                warn!(?job, "blocklist worker stopped; job dropped");
                false
            }
        }
    }

    /// Queue a blocklist `ClientMessage` and return `None`; hand any other
    /// message back untouched for the caller's own routing. Never awaits.
    pub fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        match msg {
            ClientMessage::SubscribeBlocklist { url } => {
                self.enqueue(BlocklistJob::Subscribe { url });
                None
            }
            ClientMessage::UnsubscribeBlocklist { id } => {
                self.enqueue(BlocklistJob::Unsubscribe { id });
                None
            }
            other => Some(other),
        }
    }

    /// Enqueue [`BlocklistJob::RefreshDue`] every `tick`, starting now. The
    /// loop itself never fetches.
    pub fn spawn_refresh_loop(&self, tick: Duration) -> JoinHandle<()> {
        let worker = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                worker.enqueue(BlocklistJob::RefreshDue);
            }
        })
    }
}

async fn run_job(mgr: &Arc<BlocklistsManager>, job: BlocklistJob) {
    let action = match job {
        BlocklistJob::RefreshDue => return mgr.refresh_due().await,
        BlocklistJob::Subscribe { url } => ClientMessage::SubscribeBlocklist { url },
        BlocklistJob::Unsubscribe { id } => ClientMessage::UnsubscribeBlocklist { id },
    };
    if let Err(e) = handle_blocklist_action(mgr.clone(), action).await {
        warn!(error = %e, "blocklist action failed");
    }
}

/// The bridge's blocklist background tasks: the event pump, the worker and
/// its refresh loop. Abort them with [`abort`](Self::abort) on shutdown.
pub struct BlocklistTasks {
    pub worker: BlocklistWorker,
    handles: Vec<JoinHandle<()>>,
}

impl BlocklistTasks {
    pub fn spawn(
        mgr: Arc<BlocklistsManager>,
        broadcast_tx: broadcast::Sender<ServerMessage>,
        refresh_tick: Duration,
    ) -> Self {
        let events = spawn_event_pump(mgr.clone(), broadcast_tx);
        let (worker, worker_handle) = BlocklistWorker::spawn(mgr);
        let refresh = worker.spawn_refresh_loop(refresh_tick);
        Self {
            worker,
            handles: vec![events, worker_handle, refresh],
        }
    }

    pub fn abort(&self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
