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
    /// Bytes of the snapshots the chunk left unreferenced and deleted.
    pub(super) snapshot_bytes: u64,
    /// `(ts_ms, id)` of the last row removed: where the next chunk of the
    /// same victims may start.
    pub(super) cursor: Option<(i64, i64)>,
}

impl Removed {
    /// Everything the chunk freed that counts toward its process's share.
    pub(super) fn bytes(&self) -> u64 {
        let events: u64 = self.by_process.iter().map(|(_, b)| b).sum();
        events + self.snapshot_bytes
    }
}

/// The query that picks a chunk of victims. Its parameters are the filter's
/// (none, the process, or the cutoff), then with a `cursor` the `ts_ms` and
/// `id` the previous chunk ended at, then the limit. The cursor is written so
/// the `events_ts` index seeks to its `ts_ms` instead of reading everything
/// older than the process's first live row again.
pub(super) fn pick_sql(victims: Victims<'_>, cursor: bool) -> String {
    let (filter, args) = match victims {
        Victims::Oldest => ("1", 0),
        Victims::Process(_) => ("coalesce(process_path, '') = ?1", 1),
        Victims::Before(_) => ("ts_ms < ?1", 1),
    };
    let after = if cursor {
        let (ts, id) = (args + 1, args + 2);
        format!(" AND ts_ms >= ?{ts} AND NOT (ts_ms = ?{ts} AND id <= ?{id})")
    } else {
        String::new()
    };
    let limit = args + 1 + if cursor { 2 } else { 0 };
    format!(
        "SELECT id, ts_ms, coalesce(process_path, ''), rule_hash, {} FROM events \
         WHERE {filter}{after} ORDER BY ts_ms, id LIMIT ?{limit}",
        row_bytes_sql()
    )
}

fn pick(
    conn: &Connection,
    victims: Victims<'_>,
    n: u64,
    after: Option<(i64, i64)>,
) -> rusqlite::Result<Vec<Picked>> {
    let mut args: Vec<rusqlite::types::Value> = match victims {
        Victims::Oldest => vec![],
        Victims::Process(p) => vec![p.to_string().into()],
        Victims::Before(cutoff) => vec![cutoff.into()],
    };
    if let Some((ts, id)) = after {
        args.extend([ts.into(), id.into()]);
    }
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    args.push(n.into());
    conn.prepare(&pick_sql(victims, after.is_some()))?
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
        removed.first_ts = if removed.rows == 1 {
            row.ts_ms
        } else {
            removed.first_ts.min(row.ts_ms)
        };
        removed.last_ts = removed.last_ts.max(row.ts_ms);
        removed.cursor = Some((row.ts_ms, row.id));
        match removed
            .by_process
            .iter_mut()
            .find(|(p, _)| *p == row.process)
        {
            Some((_, bytes)) => *bytes += row.bytes,
            None => removed.by_process.push((row.process.clone(), row.bytes)),
        }
    }
    let hashes: Vec<&[u8]> = picked
        .iter()
        .filter_map(|r| r.rule_hash.as_deref())
        .collect();
    removed.snapshot_bytes = delete_unreferenced(conn, &hashes)?;
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
        self.evict_victims_after(victims, limit, None)
    }

    /// [`evict_victims`](Self::evict_victims) for the victims after `after`,
    /// the `(ts_ms, id)` the previous chunk of the same victims ended at.
    pub(super) fn evict_victims_after(
        &mut self,
        victims: Victims<'_>,
        limit: u64,
        after: Option<(i64, i64)>,
    ) -> Result<(TxnReport, Removed), WriteError> {
        let n = limit.min(self.budget().rows_per_txn);
        let limits = LedgerLimits::from_budget(self.budget());
        let mut removed = Removed::default();
        let report = self.transact(TxnKind::Evict, |conn| {
            let picked = pick(conn, victims, n, after)?;
            removed = delete_chunk(conn, victims, &picked, &limits)?;
            Ok(removed.rows)
        })?;
        self.note_released(&removed);
        self.tally.removed(&removed.by_process);
        Ok((report, removed))
    }

    /// One chunk of the oldest events of all (tier 3).
    pub fn evict_chunk(&mut self) -> Result<TxnReport, WriteError> {
        self.evict_victims(Victims::Oldest, u64::MAX)
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
        if removed > 0 {
            self.vacuum_all()?;
        }
        Ok(Expiry {
            removed,
            skipped_for_jump: false,
        })
    }
}

/// The pause after eviction could not make room (D5): recording stops for
/// `delay`, which doubles on each failed attempt up to `max` and starts over
/// after a success.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pause {
    min: std::time::Duration,
    max: std::time::Duration,
    next: std::time::Duration,
    until: Option<std::time::Instant>,
}

impl Pause {
    pub(super) fn new(min: std::time::Duration, max: std::time::Duration) -> Self {
        Self {
            min,
            max,
            next: min,
            until: None,
        }
    }

    /// Whether recording is paused at `now`.
    pub(super) fn active(&self, now: std::time::Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// An attempt failed at `now`: pause for the next delay, which is
    /// returned.
    pub(super) fn enter(&mut self, now: std::time::Instant) -> std::time::Duration {
        let delay = self.next;
        self.until = Some(now + delay);
        self.next = delay.saturating_mul(2).min(self.max);
        delay
    }

    /// A batch was written: the next failure starts from the minimum again.
    /// Whether recording had been paused.
    pub(super) fn reset(&mut self) -> bool {
        let was = self.until.is_some();
        self.next = self.min;
        self.until = None;
        was
    }
}

/// When the writer next runs an age-expiry pass: every `every` after the
/// start (a clock that jumped is given a heartbeat to be noticed first).
#[derive(Debug, Clone, Copy)]
pub(super) struct Schedule {
    every: std::time::Duration,
    last: std::time::Instant,
}

impl Schedule {
    pub(super) fn new(every: std::time::Duration, start: std::time::Instant) -> Self {
        Self { every, last: start }
    }

    /// Whether a pass is due at `now`; asking again starts the next period.
    pub(super) fn due(&mut self, now: std::time::Instant) -> bool {
        let due = now.saturating_duration_since(self.last) >= self.every;
        if due {
            self.last = now;
        }
        due
    }
}
