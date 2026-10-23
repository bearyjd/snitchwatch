//! The live recorder (phase L2): the totals the banner shows, the part not yet
//! saved, the dedupe, and the writer thread that does all the disk I/O.
//!
//! Nothing here waits for the disk. The feed task only takes a short lock and
//! sends a message; the writer thread takes the unsaved part (never the
//! totals, or every save would count everything again), merges it into the
//! file under the file lock, and puts it back if the write failed.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use super::aggregate::{Aggregate, Program};
use super::store::{Backend, Notice};
use crate::bridge_runtime::PendingRow;
use crate::learning::{Clocks, SystemClocks};

/// Qualified row ids remembered for the dedupe (oldest forgotten first).
pub const SEEN_CAP: usize = 4096;

/// What the banner shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub programs_seen: usize,
    pub incomplete: bool,
    pub dropped: u64,
    pub notice: Option<Notice>,
    /// A write went ahead without the lock (last writer wins).
    pub lock_degraded: bool,
}

enum Msg {
    Dirty,
    Load,
    Flush(Sender<()>),
    Clear(Sender<()>),
}

#[derive(Default)]
struct Seen {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

impl Seen {
    /// False when `id` is already remembered.
    fn insert(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > SEEN_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
        true
    }
}

#[derive(Default)]
struct Inner {
    /// Everything this window has (loaded from disk plus recorded since).
    total: Aggregate,
    /// Recorded and not yet merged into the file.
    delta: Aggregate,
    seen: Seen,
    notice: Option<Notice>,
    lock_degraded: bool,
    /// A `Dirty` message is on its way to the writer.
    dirty: bool,
    /// The stored list is to be merged into `total` (once).
    load_wanted: bool,
}

pub struct Shared {
    inner: Mutex<Inner>,
    backend: Mutex<Option<Arc<dyn Backend>>>,
    tx: Mutex<Option<Sender<Msg>>>,
    debounce: Duration,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

impl Shared {
    pub fn new(debounce: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            backend: Mutex::new(None),
            tx: Mutex::new(None),
            debounce,
        })
    }

    /// Use `backend` and start the writer thread. Only the first call does
    /// anything.
    pub fn attach(self: &Arc<Self>, backend: Arc<dyn Backend>) {
        {
            let mut slot = lock(&self.backend);
            if slot.is_some() {
                return;
            }
            *slot = Some(backend);
        }
        let (tx, rx) = mpsc::channel();
        let weak = Arc::downgrade(self);
        let debounce = self.debounce;
        let spawned = std::thread::Builder::new()
            .name("learned-writer".into())
            .spawn(move || run(&weak, &rx, debounce));
        match spawned {
            Ok(_) => *lock(&self.tx) = Some(tx),
            Err(error) => {
                tracing::warn!(%error, "learned: the writer thread did not start");
                lock(&self.inner).notice = Some(Notice::WriteFailed);
            }
        }
        if lock(&self.inner).load_wanted {
            self.send(Msg::Load);
        }
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = lock(&self.tx).as_ref() {
            // A writer that is gone has nothing left to tell.
            let _ = tx.send(msg);
        }
    }

    /// A new window: forget everything held in memory.
    pub fn reset(&self) {
        *lock(&self.inner) = Inner::default();
    }

    /// Merge what is stored into the totals, once, on the writer thread (or
    /// as soon as there is one).
    pub fn request_load(&self) {
        lock(&self.inner).load_wanted = true;
        self.send(Msg::Load);
    }

    /// Record one answered row. False when it was recorded before.
    pub fn record(&self, qualified_id: &str, row: &PendingRow, now_ms: i64) -> bool {
        let program = Program::observed(row, now_ms);
        let first = {
            let mut inner = lock(&self.inner);
            if !inner.seen.insert(qualified_id) {
                return false;
            }
            inner.total.absorb(program.clone());
            inner.delta.absorb(program);
            !std::mem::replace(&mut inner.dirty, true)
        };
        if first {
            self.send(Msg::Dirty);
        }
        true
    }

    /// Some connections were missed.
    pub fn note_lagged(&self) {
        let first = {
            let mut inner = lock(&self.inner);
            inner.total.incomplete = true;
            inner.delta.incomplete = true;
            !std::mem::replace(&mut inner.dirty, true)
        };
        if first {
            self.send(Msg::Dirty);
        }
    }

    pub fn status(&self) -> Status {
        let inner = lock(&self.inner);
        Status {
            programs_seen: inner.total.programs_seen(),
            incomplete: inner.total.incomplete,
            dropped: inner.total.dropped(),
            notice: inner.notice,
            lock_degraded: inner.lock_degraded,
        }
    }

    /// A copy of the totals.
    pub fn snapshot(&self) -> Aggregate {
        lock(&self.inner).total.clone()
    }

    /// Save what is unsaved, soon, without waiting.
    pub fn flush(&self) {
        let (ack, _unheard) = mpsc::channel();
        self.send(Msg::Flush(ack));
    }

    /// Save what is unsaved and wait (up to `timeout`) for it. True when done;
    /// with no writer there is nothing to save.
    pub fn flush_and_wait(&self, timeout: Duration) -> bool {
        if lock(&self.tx).is_none() {
            return true;
        }
        let (ack, done) = mpsc::channel();
        self.send(Msg::Flush(ack));
        done.recv_timeout(timeout).is_ok()
    }

    /// Forget everything and delete the file.
    pub fn clear(&self) {
        self.reset();
        let (ack, _unheard) = mpsc::channel();
        self.send(Msg::Clear(ack));
    }

    fn do_load(&self) {
        let Some(backend) = lock(&self.backend).clone() else {
            return;
        };
        if !lock(&self.inner).load_wanted {
            return;
        }
        let loaded = backend.load(SystemClocks.wall_ms());
        let mut inner = lock(&self.inner);
        if !std::mem::replace(&mut inner.load_wanted, false) {
            return;
        }
        inner.total = std::mem::take(&mut inner.total).merge(loaded.aggregate);
        inner.notice = loaded.notice.or(inner.notice);
    }

    fn do_flush(&self) {
        let Some(backend) = lock(&self.backend).clone() else {
            return;
        };
        let delta = {
            let mut inner = lock(&self.inner);
            inner.dirty = false;
            std::mem::take(&mut inner.delta)
        };
        if delta == Aggregate::default() {
            return;
        }
        let result = backend.merge_write(&delta, SystemClocks.wall_ms());
        let mut inner = lock(&self.inner);
        match result {
            Ok(report) => {
                inner.lock_degraded = report.lock_degraded;
                if inner.notice == Some(Notice::WriteFailed) {
                    inner.notice = None;
                }
                inner.notice = report.notice.or(inner.notice);
            }
            Err(notice) => {
                // Nothing is lost: it is saved with the next write.
                inner.delta = delta.merge(std::mem::take(&mut inner.delta));
                inner.notice = Some(notice);
            }
        }
    }

    fn do_clear(&self) {
        let Some(backend) = lock(&self.backend).clone() else {
            return;
        };
        if let Err(notice) = backend.clear() {
            lock(&self.inner).notice = Some(notice);
        }
    }
}

/// The writer thread: loads, and saves a burst of recording in one write
/// `debounce` after its first row (or at once when asked to flush).
fn run(shared: &Weak<Shared>, rx: &Receiver<Msg>, debounce: Duration) {
    let alive = |work: &dyn Fn(&Shared)| -> bool {
        match shared.upgrade() {
            Some(shared) => {
                work(&shared);
                true
            }
            None => false,
        }
    };
    while let Ok(msg) = rx.recv() {
        let keep_going = match msg {
            Msg::Load => alive(&Shared::do_load),
            Msg::Flush(ack) => {
                let ok = alive(&Shared::do_flush);
                let _ = ack.send(());
                ok
            }
            Msg::Clear(ack) => {
                let ok = alive(&Shared::do_clear);
                let _ = ack.send(());
                ok
            }
            Msg::Dirty => debounced_flush(shared, rx, debounce),
        };
        if !keep_going {
            return;
        }
    }
    // The recorder itself was dropped (only tests do): save what is left. The
    // process-wide one lives for the whole run, so quitting flushes it
    // explicitly (`learned::flush_and_wait`, called from `main`).
    if let Some(shared) = shared.upgrade() {
        shared.do_flush();
    }
}

/// After the first unsaved row: wait out the debounce (loads and flushes that
/// arrive meanwhile are served), then save once. False when the recorder is gone.
fn debounced_flush(shared: &Weak<Shared>, rx: &Receiver<Msg>, debounce: Duration) -> bool {
    let deadline = Instant::now() + debounce;
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let Some(live) = shared.upgrade() else {
            return false;
        };
        match rx.recv_timeout(wait) {
            Ok(Msg::Dirty) => {}
            Ok(Msg::Load) => live.do_load(),
            Ok(Msg::Flush(ack)) => {
                live.do_flush();
                let _ = ack.send(());
                return true;
            }
            Ok(Msg::Clear(ack)) => {
                live.do_clear();
                let _ = ack.send(());
                return true;
            }
            Err(RecvTimeoutError::Timeout) => {
                live.do_flush();
                return true;
            }
            Err(RecvTimeoutError::Disconnected) => {
                live.do_flush();
                return false;
            }
        }
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
