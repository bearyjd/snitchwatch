//! Bounded private cache-summary jobs. No policy effects or observation-bearing logs.
use snitchwatch_bridge::{
    analysis_export::{
        self as core, ExportControl, ExportDocument, ExportError, ExportFilters, ExportRun,
        ExportSession, PreparedExport, Snapshot, WeakExportSession,
    },
    cache::connections::ConnectionCache,
    ws_messages::{valid_request_id, ClientMessage, ReplyTo, ServerMessage},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

const WORK_LIMIT: usize = 2;
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct AnalysisExports {
    inner: Arc<Inner>,
}
struct Inner {
    cache: Arc<tokio::sync::Mutex<ConnectionCache>>,
    permits: Arc<Semaphore>,
    stopped: Arc<AtomicBool>,
    state: Mutex<State>,
    hooks: Arc<dyn WorkerHooks>,
}
#[derive(Default)]
struct State {
    jobs: Vec<JoinHandle<()>>,
    sessions: Vec<WeakExportSession>,
}
trait WorkerHooks: Send + Sync {
    fn entered(&self, _control: &ExportControl) {}
    fn snapshot_ready(&self) {}
    fn exited(&self) {}
    fn published(&self) {}
    #[cfg(test)]
    fn before_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(std::future::ready(()))
    }
    fn wait_for_deadline(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline,
        )))
    }
    fn attempt(
        &self,
        snapshot: &Snapshot,
        filters: &ExportFilters,
        control: &ExportControl,
    ) -> Result<ExportDocument, ExportError> {
        let key = core::fresh_key()?;
        core::build(snapshot, filters, &key, control)
    }
}
struct Production;
impl WorkerHooks for Production {}
struct Request {
    id: String,
    filters: ExportFilters,
    reply: ReplyTo,
    session: ExportSession,
}
struct Response {
    id: String,
    reply: ReplyTo,
    run: Arc<ExportRun>,
    session: ExportSession,
    stopped: Arc<AtomicBool>,
}
impl Response {
    fn refuse(&self, reason: ExportError) {
        if !self.stopped.load(Ordering::Acquire) {
            refuse(&self.reply, &self.id, reason);
        }
    }
}
fn refuse(reply: &ReplyTo, id: &str, reason: ExportError) {
    let _ = reply.try_send(ServerMessage::CacheSummaryExportRefused {
        request_id: id.into(),
        reason,
    });
}
impl AnalysisExports {
    pub(crate) fn new(cache: Arc<tokio::sync::Mutex<ConnectionCache>>) -> Self {
        Self::with_hooks(cache, Arc::new(Production))
    }
    fn with_hooks(
        cache: Arc<tokio::sync::Mutex<ConnectionCache>>,
        hooks: Arc<dyn WorkerHooks>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                cache,
                hooks,
                permits: Arc::new(Semaphore::new(WORK_LIMIT)),
                stopped: Arc::new(AtomicBool::new(false)),
                state: Mutex::new(State::default()),
            }),
        }
    }
    pub(crate) fn try_route(&self, message: ClientMessage) -> Option<ClientMessage> {
        match message {
            ClientMessage::RequestCacheSummaryExport {
                request_id,
                schema_version,
                filters,
                reply,
                session,
            } => {
                if let (Some(reply), Some(session)) = (reply, session) {
                    if !valid_request_id(&request_id) {
                        return None;
                    }
                    if schema_version != 1 {
                        refuse(&reply, &request_id, ExportError::UnsupportedVersion);
                    } else if let Err(reason) = filters.validate() {
                        refuse(&reply, &request_id, reason);
                    } else {
                        self.start(Request {
                            id: request_id,
                            filters,
                            reply,
                            session,
                        });
                    }
                }
            }
            ClientMessage::CancelCacheSummaryExport {
                request_id,
                schema_version,
                reply,
                session,
            } => {
                if let (Some(reply), Some(session)) = (reply, session) {
                    if !valid_request_id(&request_id) {
                        return None;
                    }
                    if schema_version == 1 {
                        session.cancel(&request_id);
                    } else {
                        refuse(&reply, &request_id, ExportError::UnsupportedVersion);
                    }
                }
            }
            other => return Some(other),
        }
        None
    }
    fn start(&self, request: Request) {
        // The only lock order is service state -> session. Workers never take service state.
        let mut state = self.inner.state.lock().unwrap();
        state.jobs.retain(|job| !job.is_finished());
        state
            .sessions
            .retain(|weak| weak.upgrade().is_some_and(|session| !session.is_closed()));
        let admission = if self.inner.stopped.load(Ordering::Acquire) {
            Err(ExportError::Unavailable)
        } else if state.jobs.len() >= WORK_LIMIT {
            Err(ExportError::Busy)
        } else {
            self.inner
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| ExportError::Busy)
        };
        let permit = match admission {
            Ok(permit) => Arc::new(permit),
            Err(reason) => {
                refuse(&request.reply, &request.id, reason);
                return;
            }
        };
        let run = match request
            .session
            .admit(&request.id, Instant::now() + DEADLINE)
        {
            Ok(run) => run,
            Err(reason) => {
                refuse(&request.reply, &request.id, reason);
                return;
            }
        };
        // Keep core session admission until the worker AND old supervisor have exited.
        // A reused request ID cannot be canceled by a stale timeout generation.
        let run = Arc::new(run);
        if !state.sessions.iter().any(|weak| {
            weak.upgrade()
                .is_some_and(|session| session == request.session)
        }) {
            state.sessions.push(request.session.downgrade());
        }
        let response = Response {
            id: request.id.clone(),
            reply: request.reply,
            run: run.clone(),
            session: request.session,
            stopped: self.inner.stopped.clone(),
        };
        let cache = self.inner.cache.clone();
        let hooks = self.inner.hooks.clone();
        let worker_permit = permit.clone();
        let waiter_hooks = hooks.clone();
        let runtime = tokio::runtime::Handle::current();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let worker = tokio::task::spawn_blocking(move || {
            // Both the actual worker and its supervisor own admission. Timeout cannot free it.
            let _permit = worker_permit;
            let _exit = WorkerExit(hooks.clone());
            tracing::dispatcher::with_default(&dispatch, || {
                hooks.entered(run.control());
                let prepared =
                    runtime
                        .block_on(snapshot(&cache, run.control()))
                        .and_then(|snapshot| {
                            hooks.snapshot_ready();
                            prepare(
                                &snapshot,
                                &request.filters,
                                &request.id,
                                run.control(),
                                hooks.as_ref(),
                            )
                        })?;
                if run.publish(prepared) {
                    hooks.published();
                    Ok(())
                } else {
                    Err(run
                        .control()
                        .check()
                        .err()
                        .unwrap_or(ExportError::Unavailable))
                }
            })
        });
        state.jobs.push(tokio::spawn(supervise(
            worker,
            response,
            permit,
            waiter_hooks,
        )));
    }
    pub(crate) fn stop(&self) {
        // Synchronous shutdown signals jobs; their existing supervisors still reap actual exits.
        let mut state = self.inner.state.lock().unwrap();
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.permits.close();
        for weak in state.sessions.drain(..) {
            if let Some(session) = weak.upgrade() {
                session.close();
            }
        }
        state.jobs.retain(|job| !job.is_finished());
    }
}
struct WorkerExit(Arc<dyn WorkerHooks>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.0.exited();
    }
}

async fn snapshot(
    cache: &tokio::sync::Mutex<ConnectionCache>,
    control: &ExportControl,
) -> Result<Snapshot, ExportError> {
    control.check()?;
    let lock = tokio::time::timeout_at(
        tokio::time::Instant::from_std(control.deadline),
        cache.lock(),
    );
    tokio::pin!(lock);
    loop {
        let cache = tokio::select! {
            result = &mut lock => result.map_err(|_| ExportError::TimedOut)?,
            _ = tokio::time::sleep(Duration::from_millis(10)) => { control.check()?; continue; }
        };
        let captured_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .ok_or(ExportError::Internal)?;
        return core::capture(cache.rows(), captured_at_ms, control);
    }
}
fn prepare(
    snapshot: &Snapshot,
    filters: &ExportFilters,
    id: &str,
    control: &ExportControl,
    hooks: &dyn WorkerHooks,
) -> Result<PreparedExport, ExportError> {
    for attempt in 0..3 {
        control.check()?;
        match hooks.attempt(snapshot, filters, control) {
            Ok(document) => return core::prepare_reply(id, document, control),
            Err(ExportError::Collision) if attempt < 2 => {}
            Err(reason) => return Err(reason),
        }
    }
    Err(ExportError::Collision)
}
async fn supervise(
    worker: JoinHandle<Result<(), ExportError>>,
    response: Response,
    _permit: Arc<OwnedSemaphorePermit>,
    hooks: Arc<dyn WorkerHooks>,
) {
    let reap = async {
        let outcome = worker.await;
        #[cfg(test)]
        hooks.before_reap().await;
        outcome
    };
    tokio::pin!(reap);
    tokio::select! {
        biased;
        outcome = &mut reap => {
            match outcome { Ok(Ok(())) => {}, Ok(Err(reason)) => response.refuse(reason), Err(_) => response.refuse(ExportError::Internal) }
        }
        _ = hooks.wait_for_deadline(response.run.control().deadline) => {
            response.run.control().cancelled.store(true, Ordering::Release);
            response.session.cancel(&response.id);
            response.refuse(ExportError::TimedOut);
            // Never detach, abort, or free the last admission reference on timeout.
            let _ = reap.await;
        }
    }
}
#[cfg(test)]
mod tests;
