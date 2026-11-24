//! The age-expiry budget (PR 8, finding F1): a bucket that bounds how many
//! events tier 1 may remove, whatever the wall clock says.
//!
//! The wall clock decides which events are *old*; a clock that stepped
//! forward (wrong from boot, stepped while the bridge was down, stepped
//! between a heartbeat and a pass) makes every event old, and the jump
//! detector only skips one pass. The bucket is the independent limit: it
//! holds at most an eighth of the stored events (12.5%) and refills over 24
//! hours, so at most a quarter of the history expires in a rolling day.
//!
//! **The clock is not the wall clock.** Refill is measured by the process's
//! monotonic clock (`Instant`, `CLOCK_MONOTONIC`), which `settimeofday` and an
//! NTP step cannot move. Time while the bridge is down is *not* counted: a
//! monotonic reading means nothing across a restart, and a wall delta
//! across one is exactly the number a stepped clock forges. The cost is that
//! a store left idle refills only while the bridge runs (documented in the
//! plan). What persists, in `meta.expiry_debt_milli`, is the *debt*: thousandths
//! of a row spent and not yet refilled, as of the last spend. It is written in
//! the transaction that deletes the rows, so a crash cannot keep the deletion
//! and lose the debit, and a restart resumes with the debt it had.
//!
//! The bucket's size follows the store: `(events + debt rows) / 8`, never
//! under one chunk (`rows_per_txn`) so a small store still expires. Adding the
//! debt back approximates the event count when the window began, so
//! spending a full bucket leaves it exactly empty rather than over-drawn by
//! the shrinkage it caused.

use std::time::Instant;

use rusqlite::{Connection, OptionalExtension};

#[cfg(test)]
use super::writer::Engine;

/// A bucket refills completely in this long, of the bridge's own uptime.
const REFILL_MS: u64 = 86_400_000;
/// The bucket holds this fraction of the events: one in eight.
const DEPTH_DIVISOR: u64 = 8;
/// Debt is kept in thousandths of a row, so a refill of less than a row per
/// pass is not truncated away.
const MILLI: u64 = 1000;

#[derive(Debug)]
pub(super) struct ExpiryBudget {
    /// Debt in thousandths of a row as of `settled_ms`.
    debt_milli: u64,
    /// Uptime, in ms, the debt was last settled at.
    settled_ms: u64,
    /// The bucket's size in thousandths of a row, as of the latest pass.
    capacity_milli: u64,
    origin: Instant,
    /// Test seam: a clock the test moves by hand, in place of `origin`.
    #[cfg(test)]
    virtual_ms: Option<u64>,
}

impl ExpiryBudget {
    /// The budget with the debt `meta` holds (none: a full bucket).
    pub(super) fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let debt: Option<i64> = conn
            .query_row("SELECT expiry_debt_milli FROM meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(Self {
            debt_milli: u64::try_from(debt.unwrap_or(0)).unwrap_or(0),
            settled_ms: 0,
            capacity_milli: 0,
            origin: Instant::now(),
            #[cfg(test)]
            virtual_ms: None,
        })
    }

    /// Milliseconds of this process's monotonic uptime.
    pub(super) fn now_ms(&self) -> u64 {
        #[cfg(test)]
        if let Some(ms) = self.virtual_ms {
            return ms;
        }
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Sizes the bucket for a store of `events` rows and returns how many rows
    /// a pass may remove at uptime `now_ms` (and which `rows_per_txn` floors).
    pub(super) fn allowance(&mut self, now_ms: u64, events: u64, floor_rows: u64) -> u64 {
        let debt_rows = (self.debt_milli / MILLI).min(events);
        let capacity = ((events + debt_rows) / DEPTH_DIVISOR).max(floor_rows);
        self.capacity_milli = capacity.saturating_mul(MILLI);
        self.debt_milli = self.debt_milli.min(self.capacity_milli);
        let debt = self.debt_at(now_ms);
        (self.capacity_milli - debt) / MILLI
    }

    /// The debt at uptime `now_ms`: what is left after the refill since it was
    /// last settled. A reading that went backwards refills nothing.
    pub(super) fn debt_at(&self, now_ms: u64) -> u64 {
        let elapsed = u128::from(now_ms.saturating_sub(self.settled_ms));
        let refilled = elapsed * u128::from(self.capacity_milli) / u128::from(REFILL_MS);
        let refilled = u64::try_from(refilled).unwrap_or(u64::MAX);
        self.debt_milli.saturating_sub(refilled)
    }

    /// `debt_milli` after a chunk of `rows` events was removed.
    pub(super) fn debt_after(debt_milli: u64, rows: u64) -> u64 {
        debt_milli.saturating_add(rows.saturating_mul(MILLI))
    }

    /// The chunk committed: `debt_milli` is the new debt as of `now_ms`.
    pub(super) fn settle(&mut self, debt_milli: u64, now_ms: u64) {
        self.debt_milli = debt_milli;
        self.settled_ms = now_ms;
    }
}

/// Writes the debt in the caller's transaction. A store with no `meta` row
/// (no session ever began) keeps it in memory only.
pub(super) fn write_debt(conn: &Connection, debt_milli: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE meta SET expiry_debt_milli = ?1 WHERE id = 1",
        [i64::try_from(debt_milli).unwrap_or(i64::MAX)],
    )?;
    Ok(())
}

#[cfg(test)]
impl Engine {
    /// Stops the budget's clock at uptime 0 until [`Self::advance_expiry_clock`].
    pub(super) fn freeze_expiry_clock(&mut self) {
        self.expiry.virtual_ms = Some(0);
    }

    pub(super) fn advance_expiry_clock(&mut self, by: std::time::Duration) {
        let now = self.expiry.virtual_ms.get_or_insert(0);
        *now += u64::try_from(by.as_millis()).unwrap();
    }

    /// The debt as of now, in thousandths of a row.
    pub(super) fn expiry_debt_milli(&self) -> u64 {
        self.expiry.debt_at(self.expiry.now_ms())
    }
}
