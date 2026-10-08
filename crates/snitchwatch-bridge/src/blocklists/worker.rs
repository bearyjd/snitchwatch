//! The single blocklist worker (issue #45 PR A).
//!
//! Every subscribe and refresh becomes a [`BlocklistJob`] on one bounded
//! queue, run in order by one task:
//! - the inbound pump only enqueues ([`BlocklistWorker::try_route`] never
//!   awaits), so a fetch — up to `FETCH_TIMEOUT` — never stalls verdicts;
//! - at most one fetch, and so at most one `MAX_BODY_BYTES` body, is in
//!   flight at a time;
//! - a scheduled refresh downloads one due list, then requeues itself at the
//!   back, so user requests interleave with scheduled downloads instead of
//!   waiting behind every one of them.
//!
//! Unsubscribes take a separate lane: a set of known ids (so at most
//! `MAX_SUBSCRIPTIONS` entries) that the worker drains before each job. They
//! are never dropped and never block the caller.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::blocklists::fetcher::validate_subscription_url;
use crate::blocklists::{spawn_event_pump, BlocklistsManager};
use crate::translator::upstream::handle_blocklist_action;
use crate::ws_messages::{ClientMessage, ServerMessage, BLOCKLIST_ENTRIES_PAGE_MAX};

/// Queued jobs beyond this are refused (a subscribe is shown as refused).
pub const JOB_QUEUE_CAPACITY: usize = 64;
/// How often the refresh loop asks the worker to refresh due subscriptions.
/// Each subscription is only re-fetched once its own interval (24 h) elapses.
pub const DEFAULT_REFRESH_TICK: Duration = Duration::from_secs(15 * 60);

const QUEUE_FULL_REASON: &str = "Too many blocklist requests are waiting; try again later";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlocklistJob {
    Subscribe {
        url: String,
    },
    /// Refresh the first due list, then requeue while more are due.
    RefreshDue,
}

impl BlocklistJob {
    /// For logs: never the URL.
    fn kind(&self) -> &'static str {
        match self {
            BlocklistJob::Subscribe { .. } => "subscribe",
            BlocklistJob::RefreshDue => "refresh-due",
        }
    }
}

/// Pending unsubscribes: ids of known subscriptions, deduplicated.
#[derive(Default)]
struct UnsubscribeLane {
    ids: Mutex<BTreeSet<String>>,
    wake: Notify,
}

impl UnsubscribeLane {
    fn push(&self, id: String) {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id);
        self.wake.notify_one();
    }

    fn pop(&self) -> Option<String> {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_first()
    }
}

/// Cheap, cloneable handle to the worker's queue.
#[derive(Clone)]
pub struct BlocklistWorker {
    tx: mpsc::Sender<BlocklistJob>,
    mgr: Arc<BlocklistsManager>,
    unsubscribes: Arc<UnsubscribeLane>,
}

impl BlocklistWorker {
    /// Spawn the worker task. It ends once every handle is dropped (or when
    /// aborted).
    pub fn spawn(mgr: Arc<BlocklistsManager>) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel(JOB_QUEUE_CAPACITY);
        let unsubscribes = Arc::new(UnsubscribeLane::default());
        let lane = unsubscribes.clone();
        let worker_mgr = mgr.clone();
        let requeue = tx.downgrade();
        let handle = tokio::spawn(async move {
            loop {
                while let Some(id) = lane.pop() {
                    if let Err(e) = handle_blocklist_action(
                        worker_mgr.clone(),
                        ClientMessage::UnsubscribeBlocklist { id },
                    )
                    .await
                    {
                        warn!(error = %e, "blocklist unsubscribe failed");
                    }
                }
                tokio::select! {
                    biased;
                    _ = lane.wake.notified() => {}
                    job = rx.recv() => match job {
                        Some(job) => run_job(&worker_mgr, job, &requeue).await,
                        None => break,
                    },
                }
            }
        });
        (
            Self {
                tx,
                mgr,
                unsubscribes,
            },
            handle,
        )
    }

    /// Queue `job` without waiting. Returns false (and logs) if the queue is
    /// full or the worker is gone; a refused subscribe is shown to the user.
    pub fn enqueue(&self, job: BlocklistJob) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(job)) => {
                warn!(
                    kind = job.kind(),
                    "blocklist worker queue full; job dropped"
                );
                if let BlocklistJob::Subscribe { url } = job {
                    self.mgr.reject_subscription(&url, QUEUE_FULL_REASON);
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(job)) => {
                warn!(kind = job.kind(), "blocklist worker stopped; job dropped");
                false
            }
        }
    }

    /// Unsubscribe `id` ahead of queued jobs. An unknown id (e.g. a refused
    /// row) only resends the list.
    pub fn unsubscribe(&self, id: String) {
        if self.mgr.has_subscription(&id) {
            self.unsubscribes.push(id);
        } else {
            self.mgr.announce_subscriptions();
        }
    }

    /// Handle a blocklist `ClientMessage` and return `None`; hand any other
    /// message back untouched for the caller's own routing. Never awaits.
    /// A subscribe URL is validated before it is queued.
    pub fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        match msg {
            ClientMessage::SubscribeBlocklist { url } => {
                match validate_subscription_url(&url) {
                    Ok(_) => {
                        self.enqueue(BlocklistJob::Subscribe { url });
                    }
                    Err(reason) => self.mgr.reject_subscription(&url, &reason),
                }
                None
            }
            ClientMessage::UnsubscribeBlocklist { id } => {
                self.unsubscribe(id);
                None
            }
            ClientMessage::RequestBlocklistEntries {
                subscription_id,
                offset,
                limit,
            } => {
                // Only known ids reach the bus (and the logs): a GUI-chosen id
                // can be up to the 1 MiB message limit and contain anything.
                if self.mgr.has_subscription(&subscription_id) {
                    let limit = limit.unwrap_or(BLOCKLIST_ENTRIES_PAGE_MAX);
                    self.mgr.request_entries(&subscription_id, offset, limit);
                }
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

async fn run_job(
    mgr: &Arc<BlocklistsManager>,
    job: BlocklistJob,
    requeue: &mpsc::WeakSender<BlocklistJob>,
) {
    match job {
        BlocklistJob::Subscribe { url } => {
            let action = ClientMessage::SubscribeBlocklist { url };
            if let Err(e) = handle_blocklist_action(mgr.clone(), action).await {
                warn!(error = %e, "blocklist subscribe failed");
            }
        }
        BlocklistJob::RefreshDue => {
            let due = mgr.due_subscription_ids();
            let Some(first) = due.first() else { return };
            if let Err(e) = mgr.refresh_now(first).await {
                warn!(id = %first, error = %e, "scheduled refresh failed");
            }
            if due.len() > 1 {
                // Back of the queue: anything a GUI queued meanwhile goes first.
                // If the queue is full, the next tick picks the rest up.
                if let Some(tx) = requeue.upgrade() {
                    let _ = tx.try_send(BlocklistJob::RefreshDue);
                }
            }
        }
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

    /// True once every task has ended (after [`abort`](Self::abort)).
    pub fn is_finished(&self) -> bool {
        self.handles.iter().all(JoinHandle::is_finished)
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
