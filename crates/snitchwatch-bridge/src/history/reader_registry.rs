//! Connection history PR 9: who may read, and how a read is stopped.
//!
//! * [`ReaderRegistry`] admits at most [`MAX_READERS`] queries bridge-wide
//!   (D8), never queues (a full or closed registry refuses at once), and
//!   hands each query a [`ReaderTicket`] that the **blocking worker owns**:
//!   the slot returns only when the worker has returned (also on panic), so
//!   a cancelled query keeps its slot until it has really stopped.
//! * [`CancelToken`] is a flag plus the SQLite interrupt handle of the
//!   worker's connection, so a cancel stops a running statement.
//! * The control gate (PR 10) quiesces readers with
//!   [`ReaderRegistry::quiesce`]: close, cancel and wait for idle as one step
//!   under the state lock. Its [`ControlGuard`] reopens on drop, and only
//!   once every overlapping guard is gone; a quiesce that timed out stays
//!   closed (fail closed) until the caller decides ([`NotIdle`]). A control
//!   operation never waits behind a queued reader, because there is no queue.
//! * [`ResponseSlots`] bound the memory of pages waiting for a slow
//!   consumer: [`RESPONSE_SLOTS`] of them. A page that was never sent is
//!   dropped at [`SEND_DEADLINE`]; a page that is being sent keeps its slot
//!   until it is dropped, so memory never exceeds the slot count.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::lifecycle::{Exhausted, Generation, ReaderGate, ReaderPermit};

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
    /// Control operations that hold the registry closed (they may overlap).
    closers: usize,
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

/// One control operation's hold on the registry's closed state.
struct Closer {
    inner: Arc<Inner>,
    /// Reopen (drop this closer's count) on drop; false after a quiesce that
    /// timed out, so a forgotten error cannot reopen the registry.
    release: bool,
}

impl Closer {
    fn armed(mut self) -> Self {
        self.release = true;
        self
    }
}

impl Drop for Closer {
    fn drop(&mut self) {
        if self.release {
            let mut state = locked(&self.inner.state);
            state.closers = state.closers.saturating_sub(1);
        }
    }
}

/// A completed quiesce: no reader is running and none is admitted. Dropping
/// it reopens the registry once no other control operation holds it closed.
#[must_use = "the registry reopens when the guard is dropped"]
pub struct ControlGuard {
    _closer: Closer,
}

impl ControlGuard {
    /// Advances the generation, which only a quiesced registry may do. A
    /// reader captures the generation once, when it starts, and tags its
    /// cursor with that value: advancing while readers are still admitted
    /// would let one that starts in the gap read the old database and
    /// return a cursor tagged as the new generation, which the replacement
    /// would accept.
    pub fn advance(&self, generation: &Generation) -> Result<u64, Exhausted> {
        generation.advance()
    }
}

/// A quiesce whose workers did not stop in time. The registry stays closed:
/// dropping this leaves it so. Wait again, or give up and reopen once the
/// control operation has been refused with nothing changed.
#[must_use = "dropping this leaves the registry closed"]
pub struct NotIdle(Closer);

impl NotIdle {
    /// Waits again; on success the guard reopens on drop as usual.
    pub fn wait_again(
        self,
        registry: &ReaderRegistry,
        within: Duration,
    ) -> Result<ControlGuard, Self> {
        if registry.wait_idle(within) {
            Ok(ControlGuard {
                _closer: self.0.armed(),
            })
        } else {
            Err(self)
        }
    }

    /// Reopens, for a control operation that failed closed without touching
    /// the store.
    pub fn give_up(self) {
        drop(self.0.armed());
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
                closers: 0,
                next: 0,
                live: Vec::new(),
            }),
            idle: Condvar::new(),
        }))
    }

    /// A slot now, or a refusal now. Never waits.
    pub fn try_acquire(&self) -> Result<ReaderTicket, NotAdmitted> {
        let mut state = locked(&self.0.state);
        if state.closers > 0 {
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

    /// Closes the registry, cancels every running query and waits for their
    /// workers to return, as one step: no query is admitted between the
    /// close and the cancel, and overlapping calls count as nested closers.
    /// New queries get [`NotAdmitted::ControlOperation`] until the returned
    /// guard (and every other) is dropped. If a worker has not returned
    /// within `within`, the registry **stays closed** and [`NotIdle`] says
    /// so: the caller must fail closed, not unlink.
    pub fn quiesce(&self, within: Duration) -> Result<ControlGuard, NotIdle> {
        let tokens: Vec<CancelToken> = {
            let mut state = locked(&self.0.state);
            state.closers += 1;
            state.live.iter().map(|(_, token)| token.clone()).collect()
        };
        for token in tokens {
            token.cancel();
        }
        let closer = Closer {
            inner: Arc::clone(&self.0),
            release: false,
        };
        if self.wait_idle(within) {
            Ok(ControlGuard {
                _closer: closer.armed(),
            })
        } else {
            Err(NotIdle(closer))
        }
    }

    pub fn is_closed(&self) -> bool {
        locked(&self.0.state).closers > 0
    }

    /// True once every worker has returned its ticket; false if `within`
    /// passes first.
    fn wait_idle(&self, within: Duration) -> bool {
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
    /// The page was taken: the slot is spent and cannot be filled again, so
    /// one slot never holds more than one live page.
    spent: AtomicBool,
    /// The slot handle, plus one while a taken page is still alive: the
    /// slot's place is held until both are gone.
    owners: AtomicUsize,
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

/// A page's place. Released on drop. Past the send deadline a page that was
/// **not yet taken** is dropped, by [`ResponseSlots::sweep`] or the next
/// [`ResponseSlots::acquire`] (there is no timer): [`Self::take`] then
/// returns `None` and [`Self::reclaimed`] says so. A page that was taken is
/// the sender's memory and keeps counting against the slots until it is
/// dropped; [`Self::overdue`] tells the sender to give up and close its
/// connection.
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
        if self.reclaimed() || self.flag.spent.load(Ordering::SeqCst) {
            return Err(page);
        }
        *payload = Some(page);
        Ok(())
    }

    /// The page, to send; `None` if it was reclaimed or never filled. The
    /// slot's place stays held until the returned page is dropped.
    pub fn take(&self) -> Option<InFlight<P>> {
        let mut payload = locked(&self.flag.payload);
        // Counted before the page leaves, under the lock a sweep also takes.
        self.flag.owners.fetch_add(1, Ordering::SeqCst);
        let Some(page) = payload.take() else {
            drop(payload);
            release(&self.flag, &self.slots);
            return None;
        };
        self.flag.spent.store(true, Ordering::SeqCst);
        Some(InFlight {
            page: Some(page),
            flag: Arc::clone(&self.flag),
            slots: Arc::clone(&self.slots),
        })
    }

    /// The send deadline has passed: stop sending and close the connection.
    pub fn overdue(&self) -> bool {
        self.flag.taken.elapsed() >= self.slots.deadline
    }
}

fn release<P>(flag: &Arc<SlotFlag<P>>, slots: &SlotsInner<P>) {
    if flag.owners.fetch_sub(1, Ordering::SeqCst) == 1 {
        locked(&slots.held).retain(|f| !Arc::ptr_eq(f, flag));
    }
}

impl<P> Drop for ResponseSlot<P> {
    fn drop(&mut self) {
        release(&self.flag, &self.slots);
    }
}

/// A page that left its slot to be sent. Dropping it frees the page, then
/// the slot's place.
pub struct InFlight<P> {
    page: Option<P>,
    flag: Arc<SlotFlag<P>>,
    slots: Arc<SlotsInner<P>>,
}

impl<P> std::ops::Deref for InFlight<P> {
    type Target = P;
    fn deref(&self) -> &P {
        self.page.as_ref().expect("held until drop")
    }
}

impl<P> Drop for InFlight<P> {
    fn drop(&mut self) {
        drop(self.page.take());
        release(&self.flag, &self.slots);
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

    /// Takes back the slots whose page outlived the send deadline unsent,
    /// dropping those pages. A page already taken for sending is not
    /// touched: its memory is still live, so its place is still counted.
    pub fn sweep(&self) {
        let mut held = locked(&self.0.held);
        self.reclaim(&mut held);
    }

    fn reclaim(&self, held: &mut Vec<Arc<SlotFlag<P>>>) {
        held.retain(|flag| {
            let overdue = flag.taken.elapsed() >= self.0.deadline;
            if !overdue {
                return true;
            }
            let mut payload = locked(&flag.payload);
            if flag.owners.load(Ordering::SeqCst) > 1 {
                return true;
            }
            flag.reclaimed.store(true, Ordering::SeqCst);
            drop(payload.take());
            false
        });
    }

    /// A slot, after a [`Self::sweep`]; `None` when all are in use and none
    /// could be taken back.
    pub fn acquire(&self) -> Option<ResponseSlot<P>> {
        let mut held = locked(&self.0.held);
        self.reclaim(&mut held);
        if held.len() >= self.0.limit {
            return None;
        }
        let flag = Arc::new(SlotFlag {
            reclaimed: AtomicBool::new(false),
            taken: Instant::now(),
            payload: Mutex::new(None),
            spent: AtomicBool::new(false),
            owners: AtomicUsize::new(1),
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
