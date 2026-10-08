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
//!
//! Reconciles (issue #45 PR B) take the same lane as one coalescing request
//! of the widest [`ReconcileScope`] asked for: `Full` after each committed
//! daemon rules snapshot ([`BlocklistTasks::spawn`]'s `rules_synced`) and
//! each unsubscribe (it may free room under the total size limit), `CleanUp`
//! after each subscribe. However many requests pile up, one pass runs, after
//! any pending unsubscribes. A refresh tick runs its own single full pass
//! once its downloads are done, retrying lists the daemon refused or didn't
//! answer for, except those the tick's downloads just tried.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::blocklists::fetcher::validate_subscription_url;
use crate::blocklists::store::FetchStatus;
use crate::blocklists::{spawn_event_pump, BlocklistsManager, ReconcileScope};
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
    /// A refresh tick: refresh the first due list, then requeue
    /// [`RefreshMore`](Self::RefreshMore) while more are due; once none
    /// are, one full reconcile.
    RefreshDue,
    /// The rest of a refresh tick. `tried`: lists its downloads already
    /// installed or had refused, which its reconcile doesn't try again.
    RefreshMore {
        tried: Vec<String>,
    },
    /// The user asked to delete the leftover blocklist rules (issue #73).
    RemoveLeftovers,
}

impl BlocklistJob {
    /// For logs: never the URL.
    fn kind(&self) -> &'static str {
        match self {
            BlocklistJob::Subscribe { .. } => "subscribe",
            BlocklistJob::RefreshDue | BlocklistJob::RefreshMore { .. } => "refresh-due",
            BlocklistJob::RemoveLeftovers => "remove-leftovers",
        }
    }
}

/// Pending unsubscribes (ids of known subscriptions, deduplicated) and
/// whether a reconcile was asked for.
#[derive(Default)]
struct UnsubscribeLane {
    ids: Mutex<BTreeSet<String>>,
    /// 0: none; otherwise the widest [`ReconcileScope`] asked for.
    reconcile: AtomicU8,
    wake: Notify,
}

const RECONCILE_CLEAN_UP: u8 = 1;
const RECONCILE_FULL: u8 = 2;

impl UnsubscribeLane {
    fn request_reconcile(&self, scope: ReconcileScope) {
        let level = match scope {
            ReconcileScope::CleanUp => RECONCILE_CLEAN_UP,
            ReconcileScope::Full => RECONCILE_FULL,
        };
        self.reconcile.fetch_max(level, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn take_reconcile(&self) -> Option<ReconcileScope> {
        match self.reconcile.swap(0, Ordering::SeqCst) {
            0 => None,
            RECONCILE_CLEAN_UP => Some(ReconcileScope::CleanUp),
            _ => Some(ReconcileScope::Full),
        }
    }

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
                    // A full pass: the list's hosts may have made room under
                    // the total size limit for later lists.
                    lane.request_reconcile(ReconcileScope::Full);
                }
                if let Some(scope) = lane.take_reconcile() {
                    worker_mgr.reconcile_with(scope).await;
                    continue;
                }
                tokio::select! {
                    biased;
                    _ = lane.wake.notified() => {}
                    job = rx.recv() => match job {
                        Some(job) => run_job(&worker_mgr, job, &requeue, &lane).await,
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

    /// Reconcile the daemon's blocklist rules at the next chance (see the
    /// module doc). Never blocks; repeated requests coalesce.
    pub fn request_reconcile(&self, scope: ReconcileScope) {
        self.unsubscribes.request_reconcile(scope);
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
            ClientMessage::RemoveLeftoverBlocklistRules => {
                self.enqueue(BlocklistJob::RemoveLeftovers);
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
    lane: &UnsubscribeLane,
) {
    match job {
        BlocklistJob::Subscribe { url } => {
            let action = ClientMessage::SubscribeBlocklist { url };
            if let Err(e) = handle_blocklist_action(mgr.clone(), action).await {
                warn!(error = %e, "blocklist subscribe failed");
            }
            lane.request_reconcile(ReconcileScope::CleanUp);
        }
        BlocklistJob::RemoveLeftovers => mgr.remove_leftover_rules().await,
        BlocklistJob::RefreshDue => refresh_tick(mgr, Vec::new(), requeue).await,
        BlocklistJob::RefreshMore { tried } => refresh_tick(mgr, tried, requeue).await,
    }
}

/// One step of a refresh tick: download the first due list, then requeue the
/// rest at the back (anything a GUI queued meanwhile goes first). When none
/// is left, or the queue is full (the next tick picks the rest up), one full
/// reconcile retries lists the daemon refused or didn't answer for, except
/// those this tick just tried.
async fn refresh_tick(
    mgr: &Arc<BlocklistsManager>,
    mut tried: Vec<String>,
    requeue: &mpsc::WeakSender<BlocklistJob>,
) {
    let due: Vec<String> = mgr
        .due_subscription_ids()
        .into_iter()
        .filter(|id| !tried.contains(id))
        .collect();
    if let Some(first) = due.first() {
        match mgr.refresh_now(first).await {
            Ok(FetchStatus::Ok) => tried.push(first.clone()),
            Ok(_) => {}
            Err(e) => warn!(id = %first, error = %e, "scheduled refresh failed"),
        }
        if due.len() > 1 {
            let Some(tx) = requeue.upgrade() else { return };
            match tx.try_send(BlocklistJob::RefreshMore { tried }) {
                Ok(()) => return,
                Err(e) => match e.into_inner() {
                    BlocklistJob::RefreshMore { tried: rest } => tried = rest,
                    _ => return,
                },
            }
        }
    }
    mgr.reconcile_after_refresh(&tried).await;
}

/// The bridge's blocklist background tasks: the event pump, the worker, its
/// refresh loop and the reconcile trigger. Abort them with
/// [`abort`](Self::abort) on shutdown.
pub struct BlocklistTasks {
    pub worker: BlocklistWorker,
    handles: Vec<JoinHandle<()>>,
}

impl BlocklistTasks {
    /// `rules_synced` is the daemon rules cache's commit generation
    /// (`UiService::rules_synced`): every bump asks for a reconcile. Take it
    /// before the gRPC server starts, so no commit is missed.
    pub fn spawn(
        mgr: Arc<BlocklistsManager>,
        broadcast_tx: broadcast::Sender<ServerMessage>,
        refresh_tick: Duration,
        rules_synced: Option<watch::Receiver<u64>>,
    ) -> Self {
        let events = spawn_event_pump(mgr.clone(), broadcast_tx);
        let (worker, worker_handle) = BlocklistWorker::spawn(mgr);
        let refresh = worker.spawn_refresh_loop(refresh_tick);
        let mut handles = vec![events, worker_handle, refresh];
        if let Some(mut synced) = rules_synced {
            let trigger = worker.clone();
            handles.push(tokio::spawn(async move {
                while synced.changed().await.is_ok() {
                    trigger.request_reconcile(ReconcileScope::Full);
                }
            }));
        }
        Self { worker, handles }
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
