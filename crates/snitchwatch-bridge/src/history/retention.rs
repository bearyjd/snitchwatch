//! Connection history PR 8: retention and eviction (design D5, D6/L1, D7, S4).
//!
//! Three kinds of victim, one deletion path. [`Victims`] says which rows a
//! chunk removes; [`Engine::evict_victims`] removes at most `rows_per_txn` of
//! them, **in one transaction** (S2 keeps it short) that also
//!
//! * deletes every snapshot none of the remaining events refers to (D7,
//!   [`delete_unreferenced`]: one helper, so no victim kind can skip it), and
//! * writes the `evicted` gap row (D6), so a crash leaves both the rows and
//!   no marker, or the marker and no rows, never one without the other.
//!
//! The marker's span is what the chunk can honestly claim. A chunk that took
//! the oldest events (age, or global order) says everything up to the oldest
//! event that is left is gone: `[0, earliest remaining]`, start unknown. A
//! chunk that took one process's events from among others' says only that
//! events of the span between its first and last removed row are gone, since
//! other processes' events inside it remain.

use rusqlite::{params_from_iter, Connection};

use super::ledger::{record_gap, Gap, GapCause, LedgerLimits};
use super::measure::row_bytes_sql;
use super::snapshots::delete_unreferenced;
use super::writer::{Engine, TxnKind, TxnReport, WriteError};

/// Which rows one eviction chunk removes, oldest `(ts_ms, id)` first.
#[derive(Debug, Clone, Copy)]
pub(super) enum Victims<'a> {
    /// The oldest events of all (tier 3).
    Oldest,
    /// The oldest events of one process (tier 2). A process with no path is
    /// `""`, as admission keys it.
    Process(&'a str),
    /// Events older than the cutoff (tier 1).
    Before(i64),
}

/// A row picked for deletion.
struct Picked {
    id: i64,
    ts_ms: i64,
    process: String,
    rule_hash: Option<Vec<u8>>,
    bytes: u64,
}

/// What one chunk removed.
#[derive(Debug, Default)]
pub(super) struct Removed {
    pub(super) rows: u64,
    pub(super) first_ts: i64,
    pub(super) last_ts: i64,
    /// Event bytes per process, by [`row_bytes_sql`].
    pub(super) by_process: Vec<(String, u64)>,
}

fn pick(conn: &Connection, victims: Victims<'_>, n: u64) -> rusqlite::Result<Vec<Picked>> {
    let (filter, args): (&str, Vec<rusqlite::types::Value>) = match victims {
        Victims::Oldest => ("1", vec![]),
        Victims::Process(p) => (
            "coalesce(process_path, '') = ?1",
            vec![p.to_string().into()],
        ),
        Victims::Before(cutoff) => ("ts_ms < ?1", vec![cutoff.into()]),
    };
    let limit = args.len() + 1;
    let sql = format!(
        "SELECT id, ts_ms, coalesce(process_path, ''), rule_hash, {} FROM events WHERE {filter} \
         ORDER BY ts_ms, id LIMIT ?{limit}",
        row_bytes_sql()
    );
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    let args = args.into_iter().chain([n.into()]);
    conn.prepare(&sql)?
        .query_map(params_from_iter(args), |r| {
            Ok(Picked {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                process: r.get(2)?,
                rule_hash: r.get(3)?,
                bytes: u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
            })
        })?
        .collect()
}

/// The marker for a chunk (see the module docs).
fn marker(victims: Victims<'_>, removed: &Removed, earliest_remaining: Option<i64>) -> Gap {
    let (start_ms, end_ms) = match victims {
        Victims::Process(_) => (removed.first_ts, removed.last_ts),
        _ => (0, earliest_remaining.unwrap_or(removed.last_ts)),
    };
    Gap {
        start_ms,
        end_ms,
        start_exact: false,
        cause: GapCause::Evicted,
        count: removed.rows,
    }
}

/// Deletes `picked`, the snapshots they leave unreferenced, and writes the
/// marker; all on `conn`, so the caller's transaction makes them one.
fn delete_chunk(
    conn: &Connection,
    victims: Victims<'_>,
    picked: &[Picked],
    limits: &LedgerLimits,
) -> rusqlite::Result<Removed> {
    let mut delete = conn.prepare("DELETE FROM events WHERE id = ?1")?;
    let mut removed = Removed::default();
    for row in picked {
        delete.execute([row.id])?;
        removed.rows += 1;
        removed.first_ts = if removed.rows == 1 { row.ts_ms } else { removed.first_ts.min(row.ts_ms) };
        removed.last_ts = removed.last_ts.max(row.ts_ms);
        match removed.by_process.iter_mut().find(|(p, _)| *p == row.process) {
            Some((_, bytes)) => *bytes += row.bytes,
            None => removed.by_process.push((row.process.clone(), row.bytes)),
        }
    }
    let hashes: Vec<&[u8]> = picked
        .iter()
        .filter_map(|r| r.rule_hash.as_deref())
        .collect();
    delete_unreferenced(conn, &hashes)?;
    if removed.rows > 0 {
        let earliest: Option<i64> =
            conn.query_row("SELECT min(ts_ms) FROM events", [], |r| r.get(0))?;
        record_gap(conn, &marker(victims, &removed, earliest), limits)?;
    }
    Ok(removed)
}

impl Engine {
    /// Removes up to `rows_per_txn` of `victims` in one transaction, with
    /// their orphaned snapshots and the `evicted` marker.
    pub(super) fn evict_victims(
        &mut self,
        victims: Victims<'_>,
        limit: u64,
    ) -> Result<(TxnReport, Removed), WriteError> {
        let n = limit.min(self.budget().rows_per_txn);
        let limits = LedgerLimits::from_budget(self.budget());
        let mut removed = Removed::default();
        let report = self.transact(TxnKind::Evict, |conn| {
            let picked = pick(conn, victims, n)?;
            removed = delete_chunk(conn, victims, &picked, &limits)?;
            Ok(removed.rows)
        })?;
        self.note_released(&removed);
        Ok((report, removed))
    }

    /// One chunk of the oldest events of all (tier 3).
    pub fn evict_chunk(&mut self) -> Result<TxnReport, WriteError> {
        self.evict_victims(Victims::Oldest, u64::MAX)
            .map(|(report, _)| report)
    }

    /// One chunk of the events of `process`, oldest first (tier 2).
    pub(super) fn evict_process_chunk(
        &mut self,
        process: &str,
    ) -> Result<TxnReport, WriteError> {
        self.evict_victims(Victims::Process(process), u64::MAX)
            .map(|(report, _)| report)
    }

    /// One chunk of the events older than `cutoff_ms` (tier 1).
    pub(super) fn evict_before_chunk(&mut self, cutoff_ms: i64) -> Result<TxnReport, WriteError> {
        self.evict_victims(Victims::Before(cutoff_ms), u64::MAX)
            .map(|(report, _)| report)
    }
}

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;
/// An expiry pass removes at most this share of the events (one in ten),
/// whatever the clock says (owner decision 2026-11-24, item 4): a wall clock
/// stepped far forward cannot erase history in one pass.
const PASS_DIVISOR: u64 = 10;
/// Most processes the released-bytes log keeps: admission's ledger holds no
/// more.
const MAX_RELEASED: usize = super::admission::MAX_BUCKETS;

/// What one age-expiry pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Expiry {
    pub removed: u64,
    /// The pass was skipped because the wall clock had jumped.
    pub skipped_for_jump: bool,
}

impl Engine {
    /// Sets the age limit, in days.
    pub fn set_retention_days(&mut self, days: u16) {
        self.retention_days = days;
    }

    fn note_released(&mut self, removed: &Removed) {
        for (process, bytes) in &removed.by_process {
            if self.released.len() >= MAX_RELEASED && !self.released.contains_key(process) {
                continue;
            }
            *self.released.entry(process.clone()).or_insert(0) += bytes;
        }
    }

    /// The event bytes eviction removed per process since the last call.
    pub fn take_released(&mut self) -> Vec<(String, u64)> {
        std::mem::take(&mut self.released).into_iter().collect()
    }

    /// One age-expiry pass at wall time `now_ms` (tier 1): removes events
    /// older than the retention limit, at most a tenth of the events, in
    /// `rows_per_txn` chunks. Skipped while the clock is known to have
    /// jumped (`clock_jumped`).
    pub(super) fn expire_aged(
        &mut self,
        now_ms: i64,
        clock_jumped: bool,
    ) -> Result<Expiry, WriteError> {
        if clock_jumped {
            return Ok(Expiry {
                removed: 0,
                skipped_for_jump: true,
            });
        }
        let age = i64::from(self.retention_days).saturating_mul(DAY_MS);
        let cutoff = now_ms.saturating_sub(age);
        let total: i64 = self
            .connection()
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
        let cap = (total.max(0) as u64 / PASS_DIVISOR).max(self.budget().rows_per_txn);
        let mut removed = 0;
        while removed < cap {
            let (_, chunk) = self.evict_victims(Victims::Before(cutoff), cap - removed)?;
            if chunk.rows == 0 {
                break;
            }
            removed += chunk.rows;
        }
        Ok(Expiry {
            removed,
            skipped_for_jump: false,
        })
    }
}
