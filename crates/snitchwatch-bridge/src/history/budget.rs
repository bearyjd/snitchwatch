//! S2 and S3: the disk budget `B`, by the **full-journal fallback**. There is
//! no per-transaction page arithmetic and no claim about how many rows or how
//! many pages one transaction touches; the bound needs none.
//!
//! # The bound
//!
//! * The main database file is capped (`PRAGMA max_page_count`) at **at most
//!   half of `B`**.
//! * Room is reserved for a **full rollback journal**: SQLite journals a page
//!   the first time a transaction modifies it, and only if the page existed
//!   when the transaction began, so a journal holds at most one 4104-byte
//!   record per page of the main file, plus a header of at most one sector
//!   (4096). With `cache_spill = OFF` (set and verified by the writer) there
//!   is exactly one header, so
//!   `journal <= JOURNAL_HEADER + max_page_count * JOURNAL_RECORD`, however
//!   many rows a transaction writes and however the b-trees are shaped.
//! * Plus the settings file with its atomic replacement in flight
//!   ([`SETTINGS_PEAK_RESERVE_BYTES`]).
//!
//! `max_page_count` is the largest page count `p` for which
//! `p * PAGE <= B / 2` and
//! `p * PAGE + JOURNAL_HEADER + p * JOURNAL_RECORD + settings <= B`, so
//! `main + journal + settings <= B` always. Whatever is left over is never
//! allocated (about 0.2% of the main file, from the 8-byte record overhead).
//!
//! The writer still checks the journal's size just before `COMMIT` and rolls
//! back if it is over [`Budget::journal_limit_bytes`]; with the bound above
//! that is a tripwire for a broken assumption (a journal mode other than
//! DELETE, a spill), not a part of the argument.
//!
//! [`ROWS_PER_TXN`] and [`VACUUM_STEPS_PER_TXN`] only size batches so that a
//! transaction stays short; the disk bound does not depend on them.
//!
//! # S3: the split of `B`
//!
//! `main` (at most `B/2`, enforced by `max_page_count`; 2% of `B` of it is
//! reserved for the ledger, PR 3, and not used by events) + `journal` (a full
//! journal of `main`) + `settings` + whatever is left over.

use super::settings::SETTINGS_PEAK_RESERVE_BYTES;

pub const PAGE: u64 = 4096;
/// Journal header: one sector, 4096 at most (512 under the unix VFS's
/// default `SQLITE_IOCAP_POWERSAFE_OVERWRITE`).
pub const JOURNAL_HEADER: u64 = 4096;
/// A journal record: page number, page, checksum.
pub const JOURNAL_RECORD: u64 = 4 + PAGE + 4;

/// Rows per insert or eviction transaction. Batching only; not a disk bound.
pub const ROWS_PER_TXN: u64 = 32;
/// Incremental-vacuum steps per transaction. Batching only.
pub const VACUUM_STEPS_PER_TXN: u64 = 64;
/// Pages kept free below the ledger reserve for one batch of worst-case rows
/// to grow the file. An estimate that decides when eviction starts; if it is
/// low, an insert hits `SQLITE_FULL` and the batch is counted as lost. The
/// disk bound does not depend on it.
const GROWTH_PAGES_PER_ROW: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub total_bytes: u64,
    pub settings_bytes: u64,
    /// Room for a full journal of the main file.
    pub journal_bytes: u64,
    /// The main file's cap, `max_page_count * PAGE`.
    pub main_bytes: u64,
    /// Never allocated: `total - main - journal - settings`.
    pub slack_bytes: u64,
    /// `PRAGMA max_page_count`.
    pub max_page_count: u64,
    /// Pages reserved for the ledger inside `max_page_count`.
    pub ledger_pages: u64,
    /// Most pages of the main file a transaction can journal.
    pub journal_pages: u64,
    pub rows_per_txn: u64,
    pub vacuum_steps_per_txn: u64,
}

/// Why a budget can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BudgetError(String);

impl Budget {
    pub fn new(total_bytes: u64) -> Result<Self, BudgetError> {
        let settings_bytes = SETTINGS_PEAK_RESERVE_BYTES;
        let for_files = total_bytes
            .checked_sub(settings_bytes + JOURNAL_HEADER)
            .ok_or_else(|| BudgetError("budget too small".into()))?;
        let half = total_bytes / 2 / PAGE;
        let with_full_journal = for_files / (PAGE + JOURNAL_RECORD);
        let max_page_count = half.min(with_full_journal);
        let ledger_pages = (total_bytes / 50).div_ceil(PAGE);
        if max_page_count <= ledger_pages + ROWS_PER_TXN * GROWTH_PAGES_PER_ROW {
            return Err(BudgetError(format!(
                "a budget of {total_bytes} bytes cannot hold a batch of rows"
            )));
        }
        let main_bytes = max_page_count * PAGE;
        let journal_bytes = JOURNAL_HEADER + max_page_count * JOURNAL_RECORD;
        Ok(Self {
            total_bytes,
            settings_bytes,
            journal_bytes,
            main_bytes,
            slack_bytes: total_bytes.saturating_sub(main_bytes + journal_bytes + settings_bytes),
            max_page_count,
            ledger_pages,
            journal_pages: max_page_count,
            rows_per_txn: ROWS_PER_TXN,
            vacuum_steps_per_txn: VACUUM_STEPS_PER_TXN,
        })
    }

    /// The largest journal file a transaction can have: a full one.
    pub fn journal_limit_bytes(&self) -> u64 {
        JOURNAL_HEADER + self.journal_pages * JOURNAL_RECORD
    }

    /// Used pages (not free) above which events are evicted before the next
    /// batch: leaves room for the ledger and one batch of rows to grow into.
    pub fn high_water_pages(&self) -> u64 {
        self.max_page_count
            .saturating_sub(self.ledger_pages)
            .saturating_sub(self.rows_per_txn * GROWTH_PAGES_PER_ROW)
    }

    /// Everything the feature may have on disk at once, worst case.
    pub fn worst_case_bytes(&self) -> u64 {
        self.max_page_count * PAGE + self.journal_limit_bytes() + self.settings_bytes
    }
}
