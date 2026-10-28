//! The history writer: one thread, a queue bounded by bytes and count, and
//! batched transactions sized by [`super::budget`] (S2), eviction of the
//! oldest events (D5) and incremental vacuum (S4). No `VACUUM`: its
//! temporary copy cannot fit in `B`.
//!
//! **Never blocks a verdict.** [`Queue::try_push`] takes a mutex only to
//! append to an in-memory deque: the writer holds that mutex only to pop a
//! batch, never across database I/O, so a slow or stuck disk cannot delay a
//! caller. A full queue drops the item and counts it ([`Queue::dropped`]);
//! turning the counter into a gap row is PR 7.
//!
//! **The page budget is enforced before the batch**: [`Engine`] never takes
//! more than `rows_per_txn` rows (or `vacuum_steps_per_txn` steps) into one
//! transaction. As a tripwire for a wrong proof, it also checks the size of
//! the rollback journal just before `COMMIT` and rolls back instead of
//! committing if it is over the budget ([`WriteError::BudgetExceeded`]).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use super::budget::{Budget, COMMIT_RESERVE_PAGES, JOURNAL_RECORD};
use super::record::{ensure_snapshot, insert_event, EventRecord, SnapshotRecord};
use super::{Location, Store};

/// Default queue bounds (D5).
pub const QUEUE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const QUEUE_MAX_COUNT: usize = 4096;
/// Eviction rounds that may fail to bring usage under the high-water mark
/// before recording pauses (D5).
pub const EVICTION_ROUNDS: u32 = 3;

/// One event and the rule snapshot it refers to, if that is new.
#[derive(Debug, Clone)]
pub struct Item {
    pub event: EventRecord,
    pub snapshot: Option<SnapshotRecord>,
}

impl Item {
    /// What the item counts for against the queue's byte bound, conservatively:
    /// twice the inline size of `Item` (the deque holds spare slots, at most
    /// as many again as are used) plus the capacity of every owned
    /// allocation.
    pub fn queue_bytes(&self) -> usize {
        let e = &self.event;
        2 * std::mem::size_of::<Item>()
            + e.process_path.as_ref().map_or(0, |v| v.allocated())
            + e.dst_host.as_ref().map_or(0, |v| v.allocated())
            + e.protocol.as_ref().map_or(0, |v| v.allocated())
            + e.rule_name.as_ref().map_or(0, |v| v.allocated())
            + e.rule_class.as_ref().map_or(0, |v| v.allocated())
            + self.snapshot.as_ref().map_or(0, |s| s.snapshot.allocated())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped;

pub(crate) struct Inner {
    items: VecDeque<Item>,
    bytes: usize,
    closed: bool,
}

/// The bounded hand-off from the capture paths to the writer thread.
pub struct Queue {
    inner: Mutex<Inner>,
    ready: Condvar,
    max_bytes: usize,
    max_count: usize,
    dropped: AtomicU64,
}

impl Queue {
    pub fn new(max_bytes: usize, max_count: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                items: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            ready: Condvar::new(),
            max_bytes,
            max_count,
            dropped: AtomicU64::new(0),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(QUEUE_MAX_BYTES, QUEUE_MAX_COUNT)
    }

    /// Queue `item`, or drop and count it when either bound would be
    /// exceeded, the queue is closed, or the lock is busy. **Never waits**:
    /// the lock is taken with `try_lock`, because the writer holds it while it
    /// drains a batch and a producer on a verdict path must not stall behind
    /// that. Contention costs one dropped (and counted) event.
    pub fn try_push(&self, item: Item) -> Result<(), Dropped> {
        let size = item.queue_bytes();
        let mut inner = match self.inner.try_lock() {
            Ok(inner) => inner,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return Err(Dropped);
            }
        };
        if inner.closed
            || inner.items.len() >= self.max_count
            || inner.bytes + size > self.max_bytes
        {
            drop(inner);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(Dropped);
        }
        inner.bytes += size;
        inner.items.push_back(item);
        drop(inner);
        self.ready.notify_one();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn lock_for_test(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Items dropped because the queue was full or closed.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Up to `max` items, waiting at most `wait` for the first. Empty after
    /// the wait, or once closed and drained.
    pub fn pop_batch(&self, max: usize, wait: Duration) -> Vec<Item> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.items.is_empty() && !inner.closed {
            inner = self
                .ready
                .wait_timeout(inner, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let take = max.min(inner.items.len());
        let batch: Vec<Item> = inner.items.drain(..take).collect();
        let freed: usize = batch.iter().map(Item::queue_bytes).sum();
        inner.bytes = inner.bytes.saturating_sub(freed);
        batch
    }

    /// Refuse new items; queued ones can still be popped.
    pub fn close(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.ready.notify_all();
    }

    fn is_closed_and_empty(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.closed && inner.items.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("history write failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("history write failed: {0}")]
    Io(#[from] std::io::Error),
    /// The journal was larger than the S2 budget; the transaction was rolled
    /// back. Means the page-count proof is wrong.
    #[error("a transaction's journal ({journal} bytes) exceeded its budget ({limit})")]
    BudgetExceeded { journal: u64, limit: u64 },
    #[error("a batch of {got} rows is over the {max}-row transaction budget")]
    BatchTooLarge { got: usize, max: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnKind {
    Insert,
    Evict,
    Vacuum,
}

/// What one committed transaction did, with its journal size measured just
/// before `COMMIT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxnReport {
    pub kind: TxnKind,
    /// Rows inserted or deleted, or vacuum steps requested.
    pub units: u64,
    pub journal_bytes: u64,
}

pub type Observer = Box<dyn FnMut(&TxnReport) + Send>;

/// The synchronous core: every database write the writer thread does.
pub struct Engine {
    conn: Connection,
    budget: Budget,
    journal: PathBuf,
    observer: Option<Observer>,
}

impl Engine {
    pub fn new(store: Store, location: &Location, budget: Budget) -> Result<Self, WriteError> {
        let conn = store.into_connection();
        // The main file can never grow past its share of B (S3).
        conn.pragma_update(None, "max_page_count", budget.max_page_count as i64)?;
        // No mid-transaction spill: a spill syncs the journal and starts a
        // second header, which the page budget does not allow for (J1).
        conn.pragma_update(None, "cache_spill", "OFF")?;
        let spill: i64 = conn.query_row("PRAGMA cache_spill", [], |r| r.get(0))?;
        if spill != 0 {
            return Err(WriteError::Io(std::io::Error::other(
                "cache_spill could not be turned off",
            )));
        }
        let mut journal = location.db_path().into_os_string();
        journal.push("-journal");
        Ok(Self {
            conn,
            budget,
            journal: journal.into(),
            observer: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn set_journal_path_for_test(&mut self, path: PathBuf) {
        self.journal = path;
    }

    /// Size of the rollback journal: 0 only when there is none, which is what
    /// a transaction that has written no page looks like; any other failure
    /// to measure it is an error, never "empty".
    fn journal_len(journal: &std::path::Path) -> Result<u64, WriteError> {
        match std::fs::metadata(journal) {
            Ok(meta) => Ok(meta.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(WriteError::Io(e)),
        }
    }

    pub fn set_observer(&mut self, observer: Observer) {
        self.observer = Some(observer);
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn pragma_u64(&self, name: &str) -> Result<u64, WriteError> {
        let value: i64 = self
            .conn
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?;
        Ok(value.max(0) as u64)
    }

    /// Pages in the file that hold data (not on the freelist).
    pub fn used_pages(&self) -> Result<u64, WriteError> {
        Ok(self.pragma_u64("page_count")? - self.pragma_u64("freelist_count")?)
    }

    pub fn file_pages(&self) -> Result<u64, WriteError> {
        self.pragma_u64("page_count")
    }

    /// Run `work` in one write transaction; check the journal against the
    /// budget before committing.
    fn transact(
        &mut self,
        kind: TxnKind,
        work: impl FnOnce(&Connection) -> Result<u64, WriteError>,
    ) -> Result<TxnReport, WriteError> {
        // The journal must be within the limit less the one record `COMMIT`
        // may still add (the change counter on page 1).
        let limit = self.budget.journal_limit_bytes() - COMMIT_RESERVE_PAGES * JOURNAL_RECORD;
        let journal = self.journal.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let units = work(&tx)?;
        let journal_bytes = Self::journal_len(&journal)?;
        if journal_bytes > limit {
            drop(tx); // rolls back
            return Err(WriteError::BudgetExceeded {
                journal: journal_bytes,
                limit,
            });
        }
        tx.commit()?;
        let report = TxnReport {
            kind,
            units,
            journal_bytes,
        };
        if let Some(observer) = self.observer.as_mut() {
            observer(&report);
        }
        Ok(report)
    }

    /// Insert up to `rows_per_txn` items in one transaction.
    pub fn insert_batch(&mut self, items: &[Item]) -> Result<TxnReport, WriteError> {
        if items.len() as u64 > self.budget.rows_per_txn {
            return Err(WriteError::BatchTooLarge {
                got: items.len(),
                max: self.budget.rows_per_txn,
            });
        }
        self.transact(TxnKind::Insert, |conn| {
            for item in items {
                if let Some(snapshot) = &item.snapshot {
                    ensure_snapshot(conn, snapshot)?;
                }
                insert_event(conn, &item.event)?;
            }
            Ok(items.len() as u64)
        })
    }

    /// Delete the oldest `rows_per_txn` events and, in the same transaction,
    /// every snapshot that none of the remaining events refers to. Each
    /// deleted event can orphan at most its own snapshot, so this costs at
    /// most `rows_per_txn * row_pages` (the same as an insert).
    pub fn evict_chunk(&mut self) -> Result<TxnReport, WriteError> {
        let n = self.budget.rows_per_txn as i64;
        self.transact(TxnKind::Evict, |conn| {
            let hashes: Vec<Vec<u8>> = conn
                .prepare(
                    "SELECT DISTINCT rule_hash FROM \
                     (SELECT rule_hash FROM events ORDER BY ts_ms, id LIMIT ?1) \
                     WHERE rule_hash IS NOT NULL",
                )?
                .query_map([n], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let deleted = conn.execute(
                "DELETE FROM events WHERE id IN \
                 (SELECT id FROM events ORDER BY ts_ms, id LIMIT ?1)",
                [n],
            )?;
            for hash in hashes {
                conn.execute(
                    "DELETE FROM rules_seen WHERE rule_hash = ?1 AND NOT EXISTS \
                     (SELECT 1 FROM events WHERE events.rule_hash = ?1)",
                    [hash],
                )?;
            }
            Ok(deleted as u64)
        })
    }

    /// Return up to `vacuum_steps_per_txn` free pages to the filesystem.
    pub fn vacuum_chunk(&mut self) -> Result<TxnReport, WriteError> {
        let steps = self.budget.vacuum_steps_per_txn;
        self.transact(TxnKind::Vacuum, |conn| {
            conn.execute_batch(&format!("PRAGMA incremental_vacuum({steps});"))?;
            Ok(steps)
        })
    }

    /// Evict (and vacuum) until usage is under the high-water mark. `true`
    /// when it is; `false` after [`EVICTION_ROUNDS`] rounds that did not get
    /// there (the caller pauses recording and retries later, D5).
    pub fn make_room(&mut self) -> Result<bool, WriteError> {
        let high = self.budget.high_water_pages();
        for _ in 0..EVICTION_ROUNDS {
            if self.used_pages()? <= high {
                return Ok(true);
            }
            if self.evict_chunk()?.units == 0 {
                break;
            }
            self.vacuum_all()?;
        }
        Ok(self.used_pages()? <= high)
    }

    /// Vacuum until no free page is left, in budgeted transactions.
    pub fn vacuum_all(&mut self) -> Result<(), WriteError> {
        while self.pragma_u64("freelist_count")? > 0 {
            let before = self.file_pages()?;
            self.vacuum_chunk()?;
            if self.file_pages()? >= before {
                break;
            }
        }
        Ok(())
    }

    /// Drain `queue` once: make room, then insert one batch. `Ok(0)` when
    /// idle. On error the popped items are lost; [`Writer`] counts them.
    pub fn run_once(&mut self, queue: &Queue, wait: Duration) -> Result<usize, WriteError> {
        let batch = queue.pop_batch(self.budget.rows_per_txn as usize, wait);
        if batch.is_empty() {
            return Ok(0);
        }
        self.write_batch(&batch)?;
        Ok(batch.len())
    }

    /// Make room (evicting and vacuuming within the page budget), then insert
    /// `batch`, at most `rows_per_txn` items.
    pub fn write_batch(&mut self, batch: &[Item]) -> Result<(), WriteError> {
        if !self.make_room()? {
            return Err(WriteError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("history is over its disk budget".into()),
            )));
        }
        self.insert_batch(batch)?;
        Ok(())
    }
}

/// The writer thread and its queue.
///
/// A batch that fails to write (a full disk budget, an I/O error, a tripped
/// budget) is **lost**: its events are counted in [`Writer::lost_events`], not
/// in [`Queue::dropped`] (which counts only events refused at the door).
/// Eviction transactions that committed before the failed insert stay
/// committed: they are independent, and the oldest events they removed were
/// already due to go (the `evicted` gap row that records them is PR 7). When
/// the worker thread ends for any reason, by shutdown or a panic, the queue
/// is closed, so producers see their events dropped and counted rather than
/// queued for nobody.
pub struct Writer {
    queue: Arc<Queue>,
    failed: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// Closes the queue when the worker ends, however it ends.
struct CloseOnExit(Arc<Queue>);

impl Drop for CloseOnExit {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The worker panicked.
#[derive(Debug, thiserror::Error)]
#[error("the history writer thread panicked")]
pub struct WriterPanicked;

impl Writer {
    pub fn spawn(mut engine: Engine, queue: Arc<Queue>) -> std::io::Result<Self> {
        let failed = Arc::new(AtomicU64::new(0));
        let lost = Arc::new(AtomicU64::new(0));
        let (q, f, l) = (queue.clone(), failed.clone(), lost.clone());
        let handle = std::thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || {
                let _closer = CloseOnExit(q.clone());
                while !q.is_closed_and_empty() {
                    let batch = q.pop_batch(
                        engine.budget().rows_per_txn as usize,
                        Duration::from_millis(200),
                    );
                    if batch.is_empty() {
                        continue;
                    }
                    if let Err(e) = engine.write_batch(&batch) {
                        f.fetch_add(1, Ordering::Relaxed);
                        l.fetch_add(batch.len() as u64, Ordering::Relaxed);
                        tracing::error!(error = %e, lost = batch.len(), "history write failed");
                        // Back off so a persistent failure doesn't spin.
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            })?;
        Ok(Self {
            queue,
            failed,
            lost,
            handle: Some(handle),
        })
    }

    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    /// Batches that failed to write.
    pub fn failed_batches(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    /// Events that were popped and then lost to a failed write.
    pub fn lost_events(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }

    /// Stop accepting items, write what is queued, and join the thread.
    pub fn shutdown(mut self) -> Result<(), WriterPanicked> {
        self.queue.close();
        match self.handle.take() {
            Some(handle) => handle.join().map_err(|_| WriterPanicked),
            None => Ok(()),
        }
    }
}
