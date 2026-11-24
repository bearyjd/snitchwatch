//! Making room (D5): what the writer does when the store passes its
//! high-water mark.
//!
//! * **Hysteresis.** Eviction goes down to the low-water mark
//!   ([`Budget::low_water_pages`](super::budget::Budget::low_water_pages)), a
//!   twentieth of the file under the high one, so one eviction serves many
//!   batches instead of the next batch passing the mark again.
//! * **Tier 2 on cheap state.** The per-process scan (two aggregates over the
//!   whole table) runs only when some process could be over its share. A
//!   [`Tally`] of what each process holds in events, kept current by both
//!   insert paths and by eviction, bounds that from above; the snapshots
//!   `rules_seen` holds (at most [`MAX_SNAPSHOTS`](super::snapshots::MAX_SNAPSHOTS))
//!   are the most a process can add to it.
//! * **Progress, not a round count.** Chunks go on while each removes rows,
//!   up to [`EVICTION_ROUNDS`] chunks in all. Recording pauses only when the
//!   store is still over high water after that: a chunk that removed nothing
//!   (nothing left to evict) or the limit.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;

use super::admission::SHARE_PERCENT;
use super::measure::{estimate_bytes, row_bytes_sql};
use super::retention::Victims;
use super::writer::{Engine, Item, WriteError, EVICTION_ROUNDS};

/// Most processes the [`Tally`] follows; one more and it is dropped, so the
/// next pressure scans (never an entry silently lost).
const MAX_TALLY: usize = super::admission::MAX_BUCKETS;

/// What each process holds in event bytes ([`row_bytes_sql`]), kept between
/// scans. `None` until the first scan, and again if it outgrows
/// [`MAX_TALLY`]: unknown means scan.
#[derive(Debug, Default)]
pub(super) struct Tally {
    held: Option<HashMap<String, u64>>,
    scans: AtomicU64,
}

impl Tally {
    /// Events were committed.
    pub(super) fn inserted(&mut self, items: &[Item]) {
        for item in items {
            let process = item.event.process_path.as_ref().map_or("", |p| p.as_str());
            self.add(process, estimate_bytes(&item.event));
        }
    }

    fn add(&mut self, process: &str, bytes: u64) {
        let Some(held) = self.held.as_mut() else {
            return;
        };
        if let Some(entry) = held.get_mut(process) {
            *entry = entry.saturating_add(bytes);
        } else if held.len() < MAX_TALLY {
            held.insert(process.to_string(), bytes);
        } else {
            self.held = None;
        }
    }

    /// Events were deleted.
    pub(super) fn removed(&mut self, by_process: &[(String, u64)]) {
        let Some(held) = self.held.as_mut() else {
            return;
        };
        for (process, bytes) in by_process {
            if let Some(entry) = held.get_mut(process) {
                *entry = entry.saturating_sub(*bytes);
            }
        }
    }

    /// The most any one process can hold in events, `None` when unknown.
    pub(super) fn largest(&self) -> Option<u64> {
        let held = self.held.as_ref()?;
        Some(held.values().copied().max().unwrap_or(0))
    }

    /// A scan found `events`.
    pub(super) fn learnt(&mut self, events: &[(String, u64)]) {
        self.scans.fetch_add(1, Ordering::Relaxed);
        self.held = (events.len() <= MAX_TALLY).then(|| events.iter().cloned().collect());
    }
}

/// Runs a `(process, bytes)` query.
fn group_bytes(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<(String, u64)>> {
    conn.prepare(sql)?
        .query_map([], |r| {
            let bytes: i64 = r.get(1)?;
            Ok((r.get::<_, String>(0)?, u64::try_from(bytes).unwrap_or(0)))
        })?
        .collect()
}

impl Engine {
    /// How many full per-process scans tier 2 has run.
    pub fn tier2_scans(&self) -> u64 {
        self.tally.scans.load(Ordering::Relaxed)
    }

    /// What each process holds in the store: its events by
    /// [`row_bytes_sql`] and the snapshots only it refers to (D5). A bounded
    /// scan (owner decision 2026-11-24, item 3: no index).
    #[cfg(test)]
    pub(super) fn process_bytes(&self) -> Result<Vec<(String, u64)>, WriteError> {
        self.with_snapshots(self.event_bytes()?)
    }

    fn with_snapshots(&self, events: Vec<(String, u64)>) -> Result<Vec<(String, u64)>, WriteError> {
        let mut held: HashMap<String, u64> = events.into_iter().collect();
        // A snapshot belongs to a process only when every event that refers
        // to it is that process's (min = max).
        let snapshots = "SELECT p, sum(len) FROM (\
             SELECT min(coalesce(e.process_path, '')) AS p, \
                    max(coalesce(e.process_path, '')) AS q, \
                    length(CAST(rs.snapshot AS BLOB)) AS len \
             FROM rules_seen rs JOIN events e ON e.rule_hash = rs.rule_hash \
             GROUP BY rs.rule_hash) WHERE p = q GROUP BY p";
        for (process, bytes) in group_bytes(self.connection(), snapshots)? {
            *held.entry(process).or_insert(0) += bytes;
        }
        Ok(held.into_iter().collect())
    }

    /// What each process holds in events alone, by [`row_bytes_sql`]: what
    /// admission's ledger counts.
    pub fn event_bytes(&self) -> Result<Vec<(String, u64)>, WriteError> {
        let sql = format!(
            "SELECT coalesce(process_path, ''), sum({}) FROM events GROUP BY 1",
            row_bytes_sql()
        );
        Ok(group_bytes(self.connection(), &sql)?)
    }

    /// The most a process may hold: [`SHARE_PERCENT`] of the main file.
    pub(super) fn share_bytes(&self) -> u64 {
        (self.budget().main_bytes / 100).saturating_mul(SHARE_PERCENT)
    }

    /// Every byte of snapshot the store holds: the most they can add to one
    /// process's share.
    fn snapshot_bytes(&self) -> Result<u64, WriteError> {
        let bytes: i64 = self.connection().query_row(
            "SELECT coalesce(sum(length(CAST(snapshot AS BLOB))), 0) FROM rules_seen",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(bytes).unwrap_or(0))
    }

    /// The processes over their share, biggest first; empty without a scan
    /// when the [`Tally`] shows none can be.
    fn over_share(&mut self) -> Result<Vec<(String, u64)>, WriteError> {
        let share = self.share_bytes();
        if let Some(largest) = self.tally.largest() {
            if largest.saturating_add(self.snapshot_bytes()?) <= share {
                return Ok(Vec::new());
            }
        }
        let events = self.event_bytes()?;
        self.tally.learnt(&events);
        let mut over: Vec<(String, u64)> = self
            .with_snapshots(events)?
            .into_iter()
            .filter(|(_, bytes)| *bytes > share)
            .collect();
        over.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
        Ok(over)
    }

    /// Tier 2: trims each process over its share, the biggest first, taking
    /// its own oldest rows (and the snapshots they leave unreferenced) a
    /// chunk at a time, until it is within its share or the store is down to
    /// `low` used pages. `chunks` counts the chunks of this attempt.
    pub(super) fn evict_over_share(
        &mut self,
        low: u64,
        chunks: &mut u32,
    ) -> Result<(), WriteError> {
        let share = self.share_bytes();
        for (process, mut held) in self.over_share()? {
            let mut cursor = None;
            while held > share && *chunks < EVICTION_ROUNDS && self.used_pages()? > low {
                let (_, chunk) =
                    self.evict_victims_after(Victims::Process(&process), u64::MAX, cursor)?;
                if chunk.rows == 0 {
                    break;
                }
                *chunks += 1;
                cursor = chunk.cursor;
                held = held.saturating_sub(chunk.bytes());
            }
        }
        Ok(())
    }

    /// Evicts (and vacuums) until usage is down to the low-water mark: first
    /// each over-share process's own rows (D5), then the oldest events of
    /// all, in short chunks. `true` when under the high-water mark; `false`
    /// when [`EVICTION_ROUNDS`] chunks, or a chunk that removed nothing, did
    /// not get there (the caller pauses recording and retries later).
    pub fn make_room(&mut self) -> Result<bool, WriteError> {
        let high = self.budget().high_water_pages();
        if self.used_pages()? <= high {
            return Ok(true);
        }
        let low = self.budget().low_water_pages();
        let mut chunks = 0;
        self.evict_over_share(low, &mut chunks)?;
        while chunks < EVICTION_ROUNDS && self.used_pages()? > low {
            if self.evict_chunk()?.units == 0 {
                break;
            }
            chunks += 1;
        }
        if chunks > 0 {
            self.vacuum_all()?;
        }
        Ok(self.used_pages()? <= high)
    }
}
