//! SQLite-backed stores, following the repo's existing `rusqlite`
//! convention (`crates/snitchwatch-bridge/src/blocklists/store.rs`).
//!
//! Two logical stores with different write-privilege owners (baseline design
//! §4):
//!
//! * [`baseline::BaselineStore`] — the privileged tier's full-hash baseline
//!   cache (`baseline.db`), read by both tiers. Phase 5 only *reads/holds*
//!   the type; the expensive hashing that populates it is Phase 6 work.
//! * [`scans::ScanStore`] — the userspace tier's scan history / findings
//!   (`scans.db`), which owns the actual "what changed since last scan"
//!   state and the new/still-outstanding/resolved reconcile.

pub mod baseline;
pub mod scans;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store mutex poisoned")]
    Poisoned,
}
