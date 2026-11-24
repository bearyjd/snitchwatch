//! Connection history PR 3b: the durable half of the lifecycle (design W1).
//!
//! Every transaction here writes, together:
//!
//! * its rows (events, a gap, a session change, or nothing for a heartbeat),
//! * the watermark `min(commit time, oldest pin not released by this
//!   transaction)`, computed under the pin registry's lock
//!   ([`PinRegistry::commit_watermark`]),
//!
//! and **only after `COMMIT` returned `Ok`** releases the pins of the items it
//! made durable. A transaction that fails (or is rolled back by the budget
//! tripwire) releases nothing: the pins stay and keep holding the watermark
//! back. A kill between the commit and the release loses nothing either: pins
//! are memory, and the next start reconstructs the gap from the watermark.

use rusqlite::Connection;

use super::ledger::{record_gap, Gap, LedgerLimits, LossCounter};
use super::pins::{PinError, PinId, PinRegistry};
use super::record::insert_event;
use super::session::{self, SessionEnd};
use super::snapshots::{admit_snapshot, SnapshotLimits, SnapshotUsage};
use super::writer::{Engine, Item, TxnKind, TxnReport, WriteError};

/// Items taken from the queue together with the pins that account for them.
/// The pins are released when, and only when, the items are durable.
#[derive(Debug)]
pub struct PinnedBatch {
    pub items: Vec<Item>,
    pub pins: Vec<PinId>,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error(transparent)]
    Write(#[from] WriteError),
    /// A pin was refused before the transaction began (a handle that was
    /// already released, listed twice, or that belongs to another registry):
    /// nothing was written and no pin was released.
    #[error("history pin: {0}")]
    Pin(#[from] PinError),
}

impl Engine {
    /// One write transaction: `work`, the watermark, then the pin release.
    fn durable_txn(
        &mut self,
        registry: &PinRegistry,
        pins: &[&PinId],
        kind: TxnKind,
        work: impl FnOnce(&Connection, i64) -> Result<u64, WriteError>,
    ) -> Result<TxnReport, DurableError> {
        let (watermark, oldest_kept) = registry.commit_bounds(pins)?;
        let report = self.transact(kind, |conn| {
            let units = work(conn, watermark)?;
            session::write_watermark_capped(conn, watermark, oldest_kept)?;
            Ok(units)
        })?;
        // Only now: the rows and the watermark are durable. The pins were
        // validated by `commit_watermark`, so this cannot fail.
        registry.release_all(pins)?;
        Ok(report)
    }

    /// Writes `batch` and releases its pins in the same transaction.
    pub fn commit_pinned(
        &mut self,
        batch: &PinnedBatch,
        registry: &PinRegistry,
    ) -> Result<TxnReport, DurableError> {
        if batch.items.len() as u64 > self.budget().rows_per_txn {
            return Err(WriteError::BatchTooLarge {
                got: batch.items.len(),
                max: self.budget().rows_per_txn,
            }
            .into());
        }
        batch.items.iter().try_for_each(Item::check_snapshot)?;
        let pins: Vec<&PinId> = batch.pins.iter().collect();
        registry.check_release(&pins)?;
        self.ensure_room()?;
        let limits = SnapshotLimits::from_budget(self.budget());
        self.durable_txn(registry, &pins, TxnKind::Insert, |conn, _| {
            let mut usage = SnapshotUsage::load(conn)?;
            for item in &batch.items {
                if let Some(snapshot) = &item.snapshot {
                    admit_snapshot(conn, &limits, &mut usage, snapshot)?;
                }
                insert_event(conn, &item.event)?;
            }
            Ok(batch.items.len() as u64)
        })
    }

    /// Writes `gap` and releases `pins` in the same transaction.
    pub fn commit_gap(
        &mut self,
        gap: &Gap,
        pins: &[&PinId],
        registry: &PinRegistry,
        limits: &LedgerLimits,
    ) -> Result<TxnReport, DurableError> {
        self.durable_txn(registry, pins, TxnKind::Ledger, |conn, _| {
            record_gap(conn, gap, limits)?;
            Ok(1)
        })
    }

    /// Flushes `counter` to its aggregated gap row and releases its pin in one
    /// transaction. The counter is cleared only once that committed; `false`
    /// when there was nothing to flush.
    pub fn flush_counter(
        &mut self,
        counter: &mut LossCounter,
        registry: &PinRegistry,
        limits: &LedgerLimits,
    ) -> Result<bool, DurableError> {
        self.flush_counter_at(counter, registry, limits, registry.now())
    }

    /// [`flush_counter`](Self::flush_counter) for a gap that ended at `end_ms`.
    pub fn flush_counter_at(
        &mut self,
        counter: &mut LossCounter,
        registry: &PinRegistry,
        limits: &LedgerLimits,
        end_ms: i64,
    ) -> Result<bool, DurableError> {
        let Some((gap, pin)) = counter.pending_gap(end_ms) else {
            return Ok(false);
        };
        self.commit_gap(&gap, &[pin], registry, limits)?;
        counter.clear();
        Ok(true)
    }

    /// A commit with no rows: advances the watermark as far as the pins allow.
    pub fn heartbeat(&mut self, registry: &PinRegistry) -> Result<TxnReport, DurableError> {
        self.durable_txn(registry, &[], TxnKind::Ledger, |_, _| Ok(0))
    }

    /// Starts a session now, turning the previous one's end into a gap row.
    pub fn begin_session(
        &mut self,
        registry: &PinRegistry,
        limits: &LedgerLimits,
    ) -> Result<Option<Gap>, DurableError> {
        let now = registry.now();
        let mut gap = None;
        self.durable_txn(registry, &[], TxnKind::Ledger, |conn, watermark| {
            gap = session::start_session(conn, limits, now, watermark)?;
            Ok(1)
        })?;
        Ok(gap)
    }

    /// Ends the open session. With accepted items still uncommitted (live
    /// pins) the end is only the watermark and the kind is `Unclean`, whatever
    /// was asked: a stop must not claim an exact end it cannot back.
    pub fn end_session(
        &mut self,
        registry: &PinRegistry,
        kind: SessionEnd,
    ) -> Result<(), DurableError> {
        let kind = if registry.is_empty() {
            kind
        } else {
            SessionEnd::Unclean
        };
        self.durable_txn(registry, &[], TxnKind::Ledger, |conn, watermark| {
            session::end_session(conn, watermark, kind)?;
            Ok(1)
        })?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "durable_tests.rs"]
pub(crate) mod tests;
