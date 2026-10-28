//! S2 and S3: the disk budget `B` and the page budget of one write
//! transaction, derived from the schema and the S1 caps. Pure arithmetic: no
//! database, no I/O. `writer.rs` takes `rows_per_txn` and
//! `vacuum_steps_per_txn` from here **before** it begins a batch; nothing is
//! measured after the fact to decide it.
//!
//! # What is proved, and from what
//!
//! Source cited below is the bundled SQLite 3.46.0 amalgamation
//! (`libsqlite3-sys` 0.30.1, the one `rusqlite` 0.32 builds).
//!
//! **J1. The rollback journal holds one record per distinct pre-existing page
//! written, and one header.** `pager_write` journals a page the first time it
//! is modified only if `pgno <= dbOrigSize`; a page past the original end of
//! file is not journaled, and a page already journaled is not journaled
//! again. A *second* header would be appended if the journal were synced in
//! the middle of the transaction, which happens when the page cache spills a
//! dirty page (`syncJournal(..., 1)` from `pagerStress`, after which the next
//! record starts at a new sector-aligned header). So the engine sets
//! **`PRAGMA cache_spill = OFF`** and verifies it: the only sync is the one at
//! commit, after the last record. Then
//! `journal bytes <= HEADER + 4104 * (distinct pages modified)`; the record
//! is a 4-byte page number + 4096 + a 4-byte checksum, and the header is one
//! sector: 512 bytes under the unix VFS's default
//! `SQLITE_IOCAP_POWERSAFE_OVERWRITE` (`setSectorSize`), at most
//! `SQLITE_DEFAULT_SECTOR_SIZE` = 4096 otherwise, which is what is assumed.
//! The journal mode is DELETE (`schema.rs`), so there is no WAL. **Cost of
//! no-spill: memory.** All dirty pages stay in the cache until commit, at
//! most `journal_pages` of them (about `B/8` bytes: 32 MiB at 256 MiB, 2 MiB
//! at 16 MiB). `writer_tests.rs` checks the journal shape with
//! `synchronous=FULL` (the production setting) and a 5-page cache that would
//! force spills, and shows the same workload breaking the one-header shape
//! with spilling on.
//!
//! **J2. Pages one B-tree operation modifies.** Let `d` be the tree's depth.
//! An insert or delete modifies the leaf, then `balance()` climbs: at each
//! level `balance_nonroot` rewrites at most `NB = 3` old siblings and
//! allocates at most 2 more (`apOld[NB]`, `apNew[NB+2]`), and writes that
//! level's parent; at the root `balance_deeper` adds one page. That is
//! `6d + 1` pages. Each page allocated or freed also writes the first
//! freelist trunk page (`freePage2`, `allocateBtreePage`: page 1 and the
//! ptrmap are counted separately below); a tree operation allocates or frees
//! at most `2d + 1` pages, so `8d + 2` pages in all (`PER_TREE_OP`). The
//! trees touched are: the `events` table, `events_ts`, `events_rule_hash`
//! (three paths) and for the snapshot the `rules_seen` table and its UNIQUE
//! index (two). `secure_delete` makes a freed page be written (zeroed), but
//! it is a page already counted as an old sibling or an overflow page.
//!
//! **J3. Overflow pages.** A cell whose payload exceeds `maxLocal = U - 35`
//! keeps at least `minLocal = (U-12)*32/255 - 23 = 489` bytes locally and the
//! rest in `ceil(rest / (U-4))` overflow pages of 4092 bytes (U = 4096). The
//! largest `events` payload (every column at its S1 cap, sizes in
//! [`MAX_EVENT_PAYLOAD`]) is 4830 bytes: 2 overflow pages as a safe upper
//! bound; a `rules_seen` row (8 KiB snapshot): 2 as well.
//!
//! **J4. Depth.** Only the **interior** pages matter for depth, so the
//! argument is about them alone. It is not true that every page is a third
//! full: `balance_quick` puts a single cell on a new rightmost table leaf, and
//! redistribution can leave sparse leaves; leaf fill is not used.
//! Let `F` be the minimum number of children of a non-root interior page.
//! Claim: `F >= 40` for these trees, and `F = 32` is used. Induction over
//! operations, on interior pages only: (a) *growth*: `balance_deeper` copies
//! the root into a new child and balances it, so the new root has 2+ children
//! and the pages below come from `balance_nonroot`; (b) *contraction*:
//! `balance_shallower` only fires when the root has no cells and pulls up its
//! single child; (c) *redistribution*: `balance_nonroot` over an interior
//! level packs cells left to right up to the usable space and then levels the
//! last page against its left neighbour (~78995-79060), so with two or more
//! output pages each holds at least about half a page less one cell, and when
//! the siblings merge into one page that page holds their combined cells,
//! each sibling having been (by the hypothesis) above the `balance()`
//! threshold (`nFree * 3 <= usableSize * 2`, a third full) or the page being
//! repaired; (d) `BTREE_BULKLOAD` (which packs differently) is used only by
//! `CREATE INDEX`/`VACUUM`, never by the writer. The interior cells are small
//! (the largest, in the 16-byte `rule_hash` indexes, is 34 bytes with its
//! pointer), so a third of a 4096-byte page is at least `(4096/3 - 12)/34 =
//! 39` cells, 40 children; 32 leaves margin for rounding. A root has at
//! least 2 children, so a tree of depth `d >= 2` has at least `2 * F^(d-2)`
//! leaf pages, and since the file has at most `max_page_count` pages,
//! `d <= 2 + log_F(max_page_count / 2)`. For 256 MiB `max_page_count` is
//! 50,788 and depth 5 would need `2 * 32^3 = 65,536` leaves, so `d <= 4`;
//! this uses only interior fan-out. `writer_tests.rs` checks the interior
//! fill and the depth with `dbstat` under inserts and random deletes. **The
//! interior fill invariant is argued from the source, not machine-checked.**
//!
//! **J5. Global terms, counted once per transaction:** page 1 (change
//! counter, freelist head), and the pointer-map pages: with `auto_vacuum`,
//! every page-parent change writes an entry on one of the
//! `ceil(P / (U/5 + 1))` ptrmap pages (`ptrmapPut`), and a balance can
//! touch entries on any of them, so all of them are counted: `1 + ptrmap`.
//!
//! **J6. One incremental-vacuum step** (`incrVacuumStep`, `bCommit = 0`)
//! removes the last page from the file: if free, it is taken off the
//! freelist (the page and the trunk it hangs from are written); otherwise it
//! is relocated (`relocatePage`): the destination page (which replaces the
//! last page), the page that points to it (a parent, or the previous overflow
//! page), and the trunk page before the destination on the freelist and the
//! trunk after it; its children's ptrmap entries are in J5. At most 6 pages;
//! [`VACUUM_STEP_PAGES`] = 8 leaves two for replacement and a preceding trunk.
//!
//! Hence one transaction of `n` rows (insert with its snapshot, or the
//! eviction of an event with its orphaned snapshot, which costs the same)
//! modifies at most `1 + ptrmap + n * ROW_PAGES` distinct pages (and the budget keeps [`COMMIT_RESERVE_PAGES`] more free for commit), and a
//! vacuum transaction of `k` steps at most `1 + ptrmap + k * 8`. The writer
//! picks the largest `n` / `k` that keeps that within the journal share.
//!
//! # S3: the split of `B`
//!
//! `slack` (10%, never allocated) + `settings` (the settings file and its
//! atomic replacement, [`super::settings::SETTINGS_PEAK_RESERVE_BYTES`]) +
//! `journal` (1/8 of `B`, the largest the journal can reach) + `main` (the
//! rest, enforced by `PRAGMA max_page_count`). Of `main`, 2% of `B` is
//! reserved for the ledger (PR 3) and not used by events.

use super::caps;
use super::settings::SETTINGS_PEAK_RESERVE_BYTES;

pub const PAGE: u64 = 4096;
const U: u64 = PAGE; // usable size: no reserved bytes per page
/// Journal header: one sector, 4096 at most (see J1).
pub const JOURNAL_HEADER: u64 = 4096;
pub const JOURNAL_RECORD: u64 = 4 + PAGE + 4;

/// Minimum children of a non-root interior page that the proof uses (J4).
pub const MIN_FANOUT: u64 = 32;
/// Pages kept free of the journal for work `COMMIT` itself does after the
/// writer's last look (the change counter on page 1 is written at commit).
pub const COMMIT_RESERVE_PAGES: u64 = 1;
/// Pages per B-tree level that one balance can write (J2).
const PER_LEVEL: u64 = 6;

/// Largest `events` record, in bytes: the S1 caps plus the record header
/// (one type byte per column, two for the long text columns, and the length).
pub const MAX_EVENT_PAYLOAD: u64 = (8 // ts_ms
    + 8 // decided_ts_ms
    + caps::PROCESS_PATH
    + caps::DST_HOST
    + caps::IP_TEXT
    + 2 // dst_port
    + caps::PROTOCOL
    + 18 // action: longest of the CHECK list
    + 13 // answered_by
    + caps::RULE_NAME
    + caps::RULE_HASH
    + caps::RULE_CLASS
    + 8 // repeat_count
    + 2 // src_port
    + caps::IP_TEXT) as u64
    + 32; // header: <= 16 type bytes + 3 for the 2-byte ones + length, rounded up
pub const MAX_SNAPSHOT_PAYLOAD: u64 = caps::SNAPSHOT as u64 + caps::RULE_HASH as u64 + 8 + 16;

const MIN_LOCAL: u64 = (U - 12) * 32 / 255 - 23;

/// Overflow pages of a cell with `payload` bytes (J3), upper bound.
pub const fn overflow_pages(payload: u64) -> u64 {
    if payload <= U - 35 {
        0
    } else {
        (payload - MIN_LOCAL).div_ceil(U - 4)
    }
}

/// Deepest a tree of at most `pages` pages can be (J4).
pub fn max_depth(pages: u64) -> u64 {
    let mut depth = 1;
    let mut leaves_needed = 2; // depth 2 needs at least 2 leaves
    while leaves_needed <= pages {
        depth += 1;
        leaves_needed = leaves_needed.saturating_mul(MIN_FANOUT);
    }
    depth.max(1)
}

/// Pages one insert or delete in a tree of depth `depth` can modify (J2).
pub const fn per_tree_op(depth: u64) -> u64 {
    (PER_LEVEL + 2) * depth + 2
}

/// Pages one step of incremental vacuum can modify (J6), with margin.
pub const VACUUM_STEP_PAGES: u64 = 8;

/// Pointer-map pages in a file of `pages` pages (J5).
pub const fn ptrmap_pages(pages: u64) -> u64 {
    pages.div_ceil(U / 5 + 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub total_bytes: u64,
    pub slack_bytes: u64,
    pub settings_bytes: u64,
    pub journal_bytes: u64,
    pub main_bytes: u64,
    /// `PRAGMA max_page_count`.
    pub max_page_count: u64,
    /// Pages reserved for the ledger inside `max_page_count`.
    pub ledger_pages: u64,
    pub depth: u64,
    /// Distinct pages one transaction may modify.
    pub journal_pages: u64,
    /// J5: page 1 plus every ptrmap page.
    pub fixed_pages: u64,
    /// Pages one row operation can modify (insert or evict, with snapshot).
    pub row_pages: u64,
    pub rows_per_txn: u64,
    pub vacuum_steps_per_txn: u64,
}

/// Why a budget can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BudgetError(String);

impl Budget {
    pub fn new(total_bytes: u64) -> Result<Self, BudgetError> {
        let slack_bytes = total_bytes / 10;
        let settings_bytes = SETTINGS_PEAK_RESERVE_BYTES;
        let journal_bytes = total_bytes / 8;
        let main_bytes = total_bytes
            .checked_sub(slack_bytes + settings_bytes + journal_bytes)
            .ok_or_else(|| BudgetError("budget too small".into()))?;
        let max_page_count = main_bytes / PAGE;
        let ledger_pages = (total_bytes / 50).div_ceil(PAGE);
        let depth = max_depth(max_page_count);
        let journal_pages = journal_bytes.saturating_sub(JOURNAL_HEADER) / JOURNAL_RECORD;
        let fixed_pages = 1 + ptrmap_pages(max_page_count);
        let tree = per_tree_op(depth);
        let event = 3 * tree + overflow_pages(MAX_EVENT_PAYLOAD) * 2;
        let snapshot = 2 * tree + overflow_pages(MAX_SNAPSHOT_PAYLOAD) * 2;
        let row_pages = event + snapshot;
        let usable = journal_pages.saturating_sub(fixed_pages + COMMIT_RESERVE_PAGES);
        let rows_per_txn = usable / row_pages;
        let vacuum_steps_per_txn = usable / VACUUM_STEP_PAGES;
        if rows_per_txn == 0 {
            return Err(BudgetError(format!(
                "a budget of {total_bytes} bytes cannot hold one worst-case row per transaction"
            )));
        }
        Ok(Self {
            total_bytes,
            slack_bytes,
            settings_bytes,
            journal_bytes,
            main_bytes,
            max_page_count,
            ledger_pages,
            depth,
            journal_pages,
            fixed_pages,
            row_pages,
            rows_per_txn,
            vacuum_steps_per_txn,
        })
    }

    /// The most pages one transaction of `rows` rows can modify.
    pub fn pages_for_rows(&self, rows: u64) -> u64 {
        self.fixed_pages + rows * self.row_pages
    }

    /// The largest journal file a transaction within the budget can have.
    pub fn journal_limit_bytes(&self) -> u64 {
        JOURNAL_HEADER + self.journal_pages * JOURNAL_RECORD
    }

    /// Used pages (not free) above which events are evicted before the next
    /// batch: leaves room for one full batch to grow the file, and the ledger.
    pub fn high_water_pages(&self) -> u64 {
        self.max_page_count
            .saturating_sub(self.ledger_pages)
            .saturating_sub(self.rows_per_txn * self.row_pages)
    }

    /// Everything the feature may have on disk at once, worst case.
    pub fn worst_case_bytes(&self) -> u64 {
        self.max_page_count * PAGE
            + self.journal_limit_bytes()
            + self.settings_bytes
            + self.slack_bytes
    }
}
