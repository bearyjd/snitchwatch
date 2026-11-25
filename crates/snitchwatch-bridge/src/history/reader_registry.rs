//! Connection history PR 9: who may read, and how a read is stopped.
//!
//! * [`ReaderRegistry`] admits at most [`MAX_READERS`] queries bridge-wide
//!   (D8), never queues (a full or closed registry refuses at once), and
//!   hands each query a [`ReaderTicket`] that the **blocking worker owns**:
//!   the slot returns only when the worker has returned (also on panic), so
//!   a cancelled query keeps its slot until it has really stopped.
//! * [`CancelToken`] is a flag plus the SQLite interrupt handle of the
//!   worker's connection, so a cancel stops a running statement.
//! * The control gate (PR 10) quiesces readers with [`ReaderRegistry::close`],
//!   [`ReaderRegistry::cancel_all`], [`ReaderRegistry::wait_idle`] and, once
//!   it is done, [`ReaderRegistry::reopen`]. A control operation never waits
//!   behind a queued reader, because there is no queue.
//! * [`ResponseSlots`] bound the memory of pages waiting for a slow
//!   consumer: [`RESPONSE_SLOTS`] of them, each reclaimed after
//!   [`SEND_DEADLINE`] so one stuck client cannot starve the rest.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::lifecycle::{ReaderGate, ReaderPermit};

/// Concurrent history queries, bridge-wide (D8).
pub const MAX_READERS: usize = 2;
/// Pages that may wait to be sent at once (D8: two response slots).
pub const RESPONSE_SLOTS: usize = 2;
/// How long a page may wait for its consumer before the slot is reclaimed.
/// To be confirmed against the real socket in PR 11.
pub const SEND_DEADLINE: Duration = Duration::from_secs(5);

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Why a query was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NotAdmitted {
    /// Every slot is taken: try again soon.
    #[error("busy")]
    Busy,
    /// A clear, disable or retention change is running: do not retry-spin.
    #[error("unavailable: control operation in progress")]
    ControlOperation,
}

#[derive(Default)]
struct TokenInner {
    cancelled: AtomicBool,
    interrupt: Mutex<Option<rusqlite::InterruptHandle>>,
}

/// Cancels one query: a flag the reader checks between statements, and the
/// interrupt of the statement that is running.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<TokenInner>);

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        if let Some(handle) = locked(&self.0.interrupt).as_ref() {
            handle.interrupt();
        }
    }

    /// Registers the worker's connection so a cancel stops the statement
    /// that is running. SQLite forgets an interrupt that arrives while no
    /// statement runs, so the reader also checks [`Self::is_cancelled`]
    /// before each statement and from its progress handler.
    pub fn attach(&self, handle: rusqlite::InterruptHandle) {
        *locked(&self.0.interrupt) = Some(handle);
    }
}

struct State {
    closed: bool,
    next: u64,
    live: Vec<(u64, CancelToken)>,
}

struct Inner {
    gate: ReaderGate,
    state: Mutex<State>,
    idle: Condvar,
}

/// A query's admission. Owned by the blocking worker; dropping it (the
/// worker returning, or unwinding) frees the slot and wakes
/// [`ReaderRegistry::wait_idle`].
pub struct ReaderTicket {
    permit: Option<ReaderPermit>,
    token: CancelToken,
    id: u64,
    registry: Arc<Inner>,
}

impl ReaderTicket {
    pub fn cancel_token(&self) -> CancelToken {
        self.token.clone()
    }
}

impl Drop for ReaderTicket {
    fn drop(&mut self) {
        // The slot first, then the registration: whoever sees the registry
        // idle sees the slot free too.
        drop(self.permit.take());
        locked(&self.registry.state)
            .live
            .retain(|(id, _)| *id != self.id);
        self.registry.idle.notify_all();
    }
}

/// The bridge-wide reader admission. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct ReaderRegistry(Arc<Inner>);

impl Default for ReaderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ReaderRegistry {
    pub fn new() -> Self {
        Self::with_limit(MAX_READERS)
    }

    pub fn with_limit(limit: usize) -> Self {
        Self(Arc::new(Inner {
            gate: ReaderGate::new(limit),
            state: Mutex::new(State {
                closed: false,
                next: 0,
                live: Vec::new(),
            }),
            idle: Condvar::new(),
        }))
    }

    /// A slot now, or a refusal now. Never waits.
    pub fn try_acquire(&self) -> Result<ReaderTicket, NotAdmitted> {
        let mut state = locked(&self.0.state);
        if state.closed {
            return Err(NotAdmitted::ControlOperation);
        }
        let permit = self.0.gate.try_acquire().map_err(|_| NotAdmitted::Busy)?;
        let id = state.next;
        state.next += 1;
        let token = CancelToken::default();
        state.live.push((id, token.clone()));
        Ok(ReaderTicket {
            permit: Some(permit),
            token,
            id,
            registry: Arc::clone(&self.0),
        })
    }

    /// New queries are refused with [`NotAdmitted::ControlOperation`] until
    /// [`Self::reopen`]. Running ones are not touched.
    pub fn close(&self) {
        locked(&self.0.state).closed = true;
    }

    pub fn reopen(&self) {
        locked(&self.0.state).closed = false;
    }

    pub fn is_closed(&self) -> bool {
        locked(&self.0.state).closed
    }

    /// Cancels every running query (flag and interrupt).
    pub fn cancel_all(&self) {
        let tokens: Vec<CancelToken> = locked(&self.0.state)
            .live
            .iter()
            .map(|(_, token)| token.clone())
            .collect();
        for token in tokens {
            token.cancel();
        }
    }

    /// True once every worker has returned its ticket; false if `within`
    /// passes first (the caller must then fail closed, not unlink).
    pub fn wait_idle(&self, within: Duration) -> bool {
        let state = locked(&self.0.state);
        let (state, _) = self
            .0
            .idle
            .wait_timeout_while(state, within, |s| !s.live.is_empty())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.live.is_empty()
    }

    pub fn in_use(&self) -> usize {
        self.0.gate.in_use()
    }

    /// One client connection's view: a new query, or the connection going
    /// away, cancels the previous query.
    pub fn connection(&self) -> ConnectionReads {
        ConnectionReads {
            registry: self.clone(),
            current: Mutex::new(None),
        }
    }
}

/// What one client connection has running. Dropping it (a disconnect)
/// cancels the query in flight.
pub struct ConnectionReads {
    registry: ReaderRegistry,
    current: Mutex<Option<CancelToken>>,
}

impl ConnectionReads {
    /// Cancels this connection's previous query, then admits a new one. The
    /// previous query keeps its slot until its worker exits, so the new one
    /// can be [`NotAdmitted::Busy`]: it is refused, never queued.
    pub fn begin(&self) -> Result<ReaderTicket, NotAdmitted> {
        let mut current = locked(&self.current);
        if let Some(previous) = current.take() {
            previous.cancel();
        }
        let ticket = self.registry.try_acquire()?;
        *current = Some(ticket.cancel_token());
        Ok(ticket)
    }
}

impl Drop for ConnectionReads {
    fn drop(&mut self) {
        if let Some(token) = locked(&self.current).take() {
            token.cancel();
        }
    }
}

struct SlotFlag<P> {
    reclaimed: AtomicBool,
    taken: Instant,
    payload: Mutex<Option<P>>,
}

struct SlotsInner<P> {
    limit: usize,
    deadline: Duration,
    held: Mutex<Vec<Arc<SlotFlag<P>>>>,
}

/// The response slots: memory for pages that are built and not yet sent. A
/// slot **owns its page**, so taking a slot back at the deadline drops the
/// page, not just a counter.
pub struct ResponseSlots<P>(Arc<SlotsInner<P>>);

impl<P> Clone for ResponseSlots<P> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl Default for ResponseSlots<super::reader::Page> {
    fn default() -> Self {
        Self::new()
    }
}

/// A page's place. Released on drop. Past the send deadline the slot is
/// taken back **lazily, by the next [`ResponseSlots::acquire`]** (there is no
/// timer): its page is dropped, [`Self::take`] returns `None` and
/// [`Self::reclaimed`] says so. The sender must then close its connection.
pub struct ResponseSlot<P> {
    flag: Arc<SlotFlag<P>>,
    slots: Arc<SlotsInner<P>>,
}

impl<P> ResponseSlot<P> {
    pub fn reclaimed(&self) -> bool {
        self.flag.reclaimed.load(Ordering::SeqCst)
    }

    /// Puts the built page in the slot. Refused (the page is handed back) if
    /// the slot was already reclaimed.
    pub fn fill(&self, page: P) -> Result<(), P> {
        let mut payload = locked(&self.flag.payload);
        if self.reclaimed() {
            return Err(page);
        }
        *payload = Some(page);
        Ok(())
    }

    /// The page, to send; `None` if it was reclaimed or never filled.
    pub fn take(&self) -> Option<P> {
        locked(&self.flag.payload).take()
    }
}

impl<P> Drop for ResponseSlot<P> {
    fn drop(&mut self) {
        locked(&self.slots.held).retain(|f| !Arc::ptr_eq(f, &self.flag));
    }
}

impl ResponseSlots<super::reader::Page> {
    pub fn new() -> Self {
        Self::with_deadline(RESPONSE_SLOTS, SEND_DEADLINE)
    }
}

impl<P> ResponseSlots<P> {
    pub fn with_deadline(limit: usize, deadline: Duration) -> Self {
        Self(Arc::new(SlotsInner {
            limit,
            deadline,
            held: Mutex::new(Vec::new()),
        }))
    }

    /// A slot, taking back (and dropping the page of) any that outlived the
    /// send deadline; `None` when all are in use and none is overdue.
    pub fn acquire(&self) -> Option<ResponseSlot<P>> {
        let mut held = locked(&self.0.held);
        held.retain(|flag| {
            let overdue = flag.taken.elapsed() >= self.0.deadline;
            if overdue {
                let mut payload = locked(&flag.payload);
                flag.reclaimed.store(true, Ordering::SeqCst);
                drop(payload.take());
            }
            !overdue
        });
        if held.len() >= self.0.limit {
            return None;
        }
        let flag = Arc::new(SlotFlag {
            reclaimed: AtomicBool::new(false),
            taken: Instant::now(),
            payload: Mutex::new(None),
        });
        held.push(Arc::clone(&flag));
        Some(ResponseSlot {
            flag,
            slots: Arc::clone(&self.0),
        })
    }
}

#[cfg(test)]
#[path = "reader_registry_tests.rs"]
mod tests;
