//! Connection history PR 3b: the gap ledger (design D6, invariant L1) and the
//! in-memory loss counters whose pins W1 holds until they are flushed.
//!
//! A gap row says "coverage is missing from `start_ms` to `end_ms`": the
//! start is exact (`start_exact`) or only a bound, and `count` is how many
//! items it stands for (a lower bound). Rows carry no process fields.
//!
//! **L1.** `gaps` and `sessions` together never exceed [`LedgerLimits`]: a
//! row ceiling and a byte ceiling (the 2% ledger reserve of S3). Adjacent
//! same-cause gaps merge on write. When the ceiling is still exceeded, the
//! oldest gaps (and the sessions inside them) are **collapsed** into one
//! `uncertain` span covering all of them, with their counts summed. Nothing is
//! ever deleted without being collapsed, so no gap is lost silently.

use rusqlite::{params, Connection, OptionalExtension};

use super::budget::{Budget, PAGE};
use super::pins::{PinError, PinId, PinRegistry};
use super::schema::GAP_CAUSES;

/// A conservative bound on the stored size of one `gaps` or `sessions` row:
/// five 8-byte integers, the longest cause (19 bytes), the record header and
/// the cell overhead. Checked against a worst-case row by a test.
pub const ROW_BYTES: u64 = 128;
/// Default row ceiling (gaps and sessions together).
pub const DEFAULT_MAX_ROWS: u64 = 2048;
/// The smallest ceiling that can hold one collapsed span, the open session
/// and a few recent rows.
pub const MIN_CEILING: u64 = 8;
/// Clock-jump rows closer than this merge into one, whatever rows came
/// between: a flapping clock must not write a row per heartbeat.
const CLOCK_JUMP_COALESCE_MS: i64 = 600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapCause {
    BridgeDown,
    DaemonDisconnected,
    UpstreamLoss,
    QueueOverflow,
    AdmissionDrop,
    WriteFailed,
    Disabled,
    Evicted,
    ClockJump,
    Uncertain,
}

const ALL_CAUSES: [GapCause; 10] = [
    GapCause::BridgeDown,
    GapCause::DaemonDisconnected,
    GapCause::UpstreamLoss,
    GapCause::QueueOverflow,
    GapCause::AdmissionDrop,
    GapCause::WriteFailed,
    GapCause::Disabled,
    GapCause::Evicted,
    GapCause::ClockJump,
    GapCause::Uncertain,
];

impl GapCause {
    pub fn as_str(self) -> &'static str {
        GAP_CAUSES[self as usize]
    }

    pub fn parse(text: &str) -> Option<Self> {
        ALL_CAUSES.into_iter().find(|c| c.as_str() == text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start_ms: i64,
    pub end_ms: i64,
    /// False: the gap began somewhere at or after `start_ms` (a crash).
    pub start_exact: bool,
    pub cause: GapCause,
    /// Items (or restarts) this gap stands for, at least.
    pub count: u64,
}

impl Gap {
    /// The view must not claim exact bounds for this one.
    pub fn is_uncertain(&self) -> bool {
        !self.start_exact || self.cause == GapCause::Uncertain
    }
}

/// L1's two ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerLimits {
    pub max_rows: u64,
    pub max_bytes: u64,
}

impl LedgerLimits {
    /// The ledger's share of `budget` (S3: 2% of `B`, in pages) and the
    /// default row ceiling.
    pub fn from_budget(budget: &Budget) -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            max_bytes: budget.ledger_pages * PAGE,
        }
    }

    /// The most rows the ledger may hold: the row ceiling, or what the byte
    /// ceiling pays for, whichever is smaller (never below [`MIN_CEILING`]).
    pub fn ceiling(&self) -> u64 {
        self.max_rows
            .min(self.max_bytes / ROW_BYTES)
            .max(MIN_CEILING)
    }
}

pub(crate) fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The ledger could not be brought within its limits (an invariant of the
/// session bookkeeping is broken). The transaction rolls back, so no pin is
/// released and nothing is lost silently.
#[derive(Debug, thiserror::Error)]
#[error("the history ledger is over its {ceiling}-row ceiling after collapsing ({rows} rows)")]
pub struct LedgerOverflow {
    ceiling: u64,
    rows: u64,
}

#[derive(Debug, Clone, Copy)]
struct GapRow {
    id: i64,
    gap: Gap,
}

/// Records `gap` (merging it into the latest gap when adjacent and of the same
/// cause; a clock jump merges into the latest clock-jump row within
/// [`CLOCK_JUMP_COALESCE_MS`], other rows in between or not), then brings the
/// ledger within `limits` by collapsing (L1).
pub fn record_gap(conn: &Connection, gap: &Gap, limits: &LedgerLimits) -> rusqlite::Result<()> {
    let target = if gap.cause == GapCause::ClockJump {
        latest_of_cause(conn, GapCause::ClockJump)?.filter(|p| near(&p.gap, gap))
    } else {
        latest_gap(conn)?.filter(|p| mergeable(&p.gap, gap))
    };
    match target {
        Some(prev) => merge_into(conn, &prev, gap)?,
        None => insert_gap(conn, gap)?,
    }
    enforce_limits(conn, limits)
}

fn near(prev: &Gap, new: &Gap) -> bool {
    new.start_ms <= prev.end_ms.saturating_add(CLOCK_JUMP_COALESCE_MS)
        && new.end_ms.saturating_add(CLOCK_JUMP_COALESCE_MS) >= prev.start_ms
}

fn mergeable(prev: &Gap, new: &Gap) -> bool {
    prev.cause == new.cause
        && prev.start_exact == new.start_exact
        && new.start_ms <= prev.end_ms
        && new.end_ms >= prev.start_ms
}

fn merge_into(conn: &Connection, prev: &GapRow, new: &Gap) -> rusqlite::Result<()> {
    let end = prev.gap.end_ms.max(new.end_ms);
    conn.execute(
        "UPDATE gaps SET start_ms = ?1, end_ms = ?2, count = ?3 WHERE id = ?4",
        params![
            prev.gap.start_ms.min(new.start_ms),
            end,
            to_i64(prev.gap.count.saturating_add(new.count)),
            prev.id
        ],
    )?;
    // Sessions that now lie wholly inside the gap (restarts within one
    // millisecond) carry no coverage; the newest session is kept, since the
    // next start reads it.
    delete_closed_sessions_inside(conn, prev.gap.end_ms, end)
}

fn delete_closed_sessions_inside(conn: &Connection, from: i64, to: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM sessions WHERE end_ms IS NOT NULL AND start_ms >= ?1 AND end_ms <= ?2 \
         AND id <> (SELECT max(id) FROM sessions)",
        [from, to],
    )?;
    Ok(())
}

fn insert_gap(conn: &Connection, gap: &Gap) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO gaps (start_ms, end_ms, start_exact, cause, count) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            gap.start_ms,
            gap.end_ms,
            gap.start_exact,
            gap.cause.as_str(),
            to_i64(gap.count)
        ],
    )?;
    Ok(())
}

fn gap_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<GapRow> {
    let cause: String = r.get(4)?;
    Ok(GapRow {
        id: r.get(0)?,
        gap: Gap {
            start_ms: r.get(1)?,
            end_ms: r.get(2)?,
            start_exact: r.get(3)?,
            // The CHECK constraint admits only known causes; an unknown one
            // can only mean a newer schema, which is read as uncertain.
            cause: GapCause::parse(&cause).unwrap_or(GapCause::Uncertain),
            count: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
        },
    })
}

const GAP_COLUMNS: &str = "id, start_ms, end_ms, start_exact, cause, count";

fn latest_gap(conn: &Connection) -> rusqlite::Result<Option<GapRow>> {
    conn.query_row(
        &format!("SELECT {GAP_COLUMNS} FROM gaps ORDER BY id DESC LIMIT 1"),
        [],
        gap_from_row,
    )
    .optional()
}

fn latest_of_cause(conn: &Connection, cause: GapCause) -> rusqlite::Result<Option<GapRow>> {
    conn.query_row(
        &format!("SELECT {GAP_COLUMNS} FROM gaps WHERE cause = ?1 ORDER BY id DESC LIMIT 1"),
        [cause.as_str()],
        gap_from_row,
    )
    .optional()
}

fn all_gap_rows(conn: &Connection) -> rusqlite::Result<Vec<GapRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {GAP_COLUMNS} FROM gaps ORDER BY id"))?;
    let rows = stmt.query_map([], gap_from_row)?;
    rows.collect()
}

/// What a collapse replaces: the oldest `gaps` rows and the closed sessions
/// inside their span, by one `uncertain` span.
#[derive(Debug, PartialEq, Eq)]
struct Collapse {
    start_ms: i64,
    end_ms: i64,
    count: u64,
    first_id: i64,
    gap_ids: Vec<i64>,
}

/// A closed session that is not the newest: `(id, start_ms, end_ms)`.
type ClosedSession = (i64, i64, i64);

/// Closed sessions wholly inside the span of the oldest `k` gaps.
fn inside_span(gaps: &[GapRow], sessions: &[ClosedSession], k: usize) -> usize {
    let (start, end) = span(&gaps[..k]);
    sessions
        .iter()
        .filter(|(_, s, e)| *s >= start && *e <= end)
        .count()
}

fn span(gaps: &[GapRow]) -> (i64, i64) {
    let start = gaps.iter().map(|g| g.gap.start_ms).min().unwrap_or(0);
    let end = gaps.iter().map(|g| g.gap.end_ms).max().unwrap_or(0);
    (start, end)
}

/// The smallest collapse of the oldest gaps that leaves at most `target`
/// rows, or all of them when none does. When the sessions inside their span
/// are too few (a clock that stepped back leaves old sessions outside every
/// gap's span), the oldest closed sessions by id are absorbed too and the span
/// widens to cover them, so the ledger can always be brought down.
/// `None` when there is no gap.
fn plan_collapse(
    total: u64,
    gaps: &[GapRow],
    sessions: &[ClosedSession],
    target: u64,
) -> Option<Collapse> {
    if gaps.is_empty() {
        return None;
    }
    let rows_after = |k: usize| total - k as u64 + 1 - inside_span(gaps, sessions, k) as u64;
    let k = (1..=gaps.len())
        .find(|&k| rows_after(k) <= target)
        .unwrap_or(gaps.len());
    let chosen = &gaps[..k];
    let (mut start_ms, mut end_ms) = span(chosen);
    let extra = rows_after(k).saturating_sub(target) as usize;
    let (lo, hi) = (start_ms, end_ms);
    for (_, s, e) in sessions
        .iter()
        .filter(|(_, s, e)| !(*s >= lo && *e <= hi))
        .take(extra)
    {
        start_ms = start_ms.min(*s);
        end_ms = end_ms.max(*e);
    }
    Some(Collapse {
        start_ms,
        end_ms,
        count: chosen
            .iter()
            .fold(0u64, |n, g| n.saturating_add(g.gap.count)),
        first_id: chosen[0].id,
        gap_ids: chosen.iter().map(|g| g.id).collect(),
    })
}

/// Brings the ledger within `limits` (L1): while rows exceed the ceiling, the
/// oldest gaps are collapsed into one `uncertain` span, which keeps the id of
/// the oldest of them (so it stays the oldest) and the sum of their counts.
/// Nothing is deleted without being covered by that span.
pub fn enforce_limits(conn: &Connection, limits: &LedgerLimits) -> rusqlite::Result<()> {
    let ceiling = limits.ceiling();
    let total = row_count(conn)?;
    if total <= ceiling {
        return Ok(());
    }
    let gaps = all_gap_rows(conn)?;
    let sessions = closed_sessions_but_newest(conn)?;
    let target = (ceiling * 3 / 4).max(MIN_CEILING / 2);
    if let Some(plan) = plan_collapse(total, &gaps, &sessions, target) {
        apply(conn, &plan)?;
    }
    let rows = row_count(conn)?;
    if rows > ceiling {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            LedgerOverflow { ceiling, rows },
        )));
    }
    Ok(())
}

/// Oldest first, by id.
fn closed_sessions_but_newest(conn: &Connection) -> rusqlite::Result<Vec<ClosedSession>> {
    let mut stmt = conn.prepare(
        "SELECT id, start_ms, end_ms FROM sessions WHERE end_ms IS NOT NULL \
         AND id <> (SELECT max(id) FROM sessions) ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    rows.collect()
}

fn apply(conn: &Connection, plan: &Collapse) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE gaps SET start_ms = ?1, end_ms = ?2, start_exact = 0, cause = ?3, count = ?4 \
         WHERE id = ?5",
        params![
            plan.start_ms,
            plan.end_ms,
            GapCause::Uncertain.as_str(),
            to_i64(plan.count),
            plan.first_id
        ],
    )?;
    for id in plan.gap_ids.iter().filter(|id| **id != plan.first_id) {
        conn.execute("DELETE FROM gaps WHERE id = ?1", [id])?;
    }
    delete_closed_sessions_inside(conn, plan.start_ms, plan.end_ms)
}

/// Every gap, oldest row first.
pub fn read_gaps(conn: &Connection) -> rusqlite::Result<Vec<Gap>> {
    Ok(all_gap_rows(conn)?.into_iter().map(|r| r.gap).collect())
}

/// Rows in `gaps` plus rows in `sessions`.
pub fn row_count(conn: &Connection) -> rusqlite::Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT (SELECT count(*) FROM gaps) + (SELECT count(*) FROM sessions)",
        [],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// An in-memory loss tally (admission drops, queue overflow, upstream loss)
/// and the pin W1 holds from the first loss until the tally is flushed to its
/// aggregated gap row, in one transaction ([`super::durable`]).
#[derive(Debug)]
pub struct LossCounter {
    cause: GapCause,
    /// False: the loss began somewhere after its recorded start (see
    /// [`Self::bounded`]).
    exact: bool,
    pending: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    count: u64,
    /// When the first loss was noticed: the pin's arrival.
    first_ms: i64,
    /// Where the gap starts: `first_ms`, or earlier when the losses are known
    /// to have begun before they were noticed.
    start_ms: i64,
    pin: PinId,
}

impl LossCounter {
    pub fn new(cause: GapCause) -> Self {
        Self {
            cause,
            exact: true,
            pending: None,
        }
    }

    /// A tally whose gap start is only a bound: the losses happened somewhere
    /// after it ([`Self::record_since`]).
    pub fn bounded(cause: GapCause) -> Self {
        Self {
            exact: false,
            ..Self::new(cause)
        }
    }

    /// Counts `n` more losses. The first one takes a pin arriving now.
    pub fn record(&mut self, registry: &PinRegistry, n: u64) -> Result<(), PinError> {
        match self.pending.as_mut() {
            Some(p) => p.count = p.count.saturating_add(n),
            None => {
                let (pin, first_ms) = registry.accept_timed()?;
                self.pending = Some(Pending {
                    count: n,
                    first_ms,
                    start_ms: first_ms,
                    pin,
                });
            }
        }
        Ok(())
    }

    /// [`record`](Self::record) for losses known to lie after `since_ms`
    /// (earlier than they were noticed): the gap starts there. The pin still
    /// arrives now, which is what W1 can honour.
    pub fn record_since(
        &mut self,
        registry: &PinRegistry,
        n: u64,
        since_ms: i64,
    ) -> Result<(), PinError> {
        self.record(registry, n)?;
        if let Some(p) = self.pending.as_mut() {
            p.start_ms = p.start_ms.min(since_ms);
        }
        Ok(())
    }

    /// Transfer an already pinned loss into the tally without advancing its
    /// coverage boundary to the later time at which the loss was detected.
    pub(super) fn record_pinned(&mut self, registry: &PinRegistry, n: u64, pin: PinId) {
        let Some(first_ms) = registry.arrival_of(&pin) else {
            return;
        };
        self.merge(
            registry,
            Pending {
                count: n,
                first_ms,
                start_ms: first_ms,
                pin,
            },
        );
    }

    /// Moves `other`'s tally into this one, keeping the older pin (W1) and
    /// releasing the other. Puts a flush that failed back.
    pub(crate) fn absorb(&mut self, registry: &PinRegistry, other: Self) {
        if let Some(pending) = other.pending {
            self.merge(registry, pending);
        }
    }

    fn merge(&mut self, registry: &PinRegistry, incoming: Pending) {
        let merged = match self.pending.take() {
            Some(mut kept) if kept.first_ms <= incoming.first_ms => {
                kept.count = kept.count.saturating_add(incoming.count);
                kept.start_ms = kept.start_ms.min(incoming.start_ms);
                let _ = registry.release(&incoming.pin);
                kept
            }
            Some(kept) => {
                let _ = registry.release(&kept.pin);
                Pending {
                    count: kept.count.saturating_add(incoming.count),
                    start_ms: kept.start_ms.min(incoming.start_ms),
                    ..incoming
                }
            }
            None => incoming,
        };
        self.pending = Some(merged);
    }

    /// Moves the tally out, leaving this one empty, so that its flush can
    /// run without the lock that guards new losses.
    pub(crate) fn take(&mut self) -> Self {
        Self {
            cause: self.cause,
            exact: self.exact,
            pending: self.pending.take(),
        }
    }

    /// Whether a tally (and so a pin) is waiting to be flushed.
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Unflushed losses, if any.
    pub fn pending_count(&self) -> u64 {
        self.pending.as_ref().map_or(0, |p| p.count)
    }

    /// The gap this tally would become if flushed at `end_ms`, with its pin.
    pub(crate) fn pending_gap(&self, end_ms: i64) -> Option<(Gap, &PinId)> {
        self.pending.as_ref().map(|p| {
            (
                Gap {
                    start_ms: p.start_ms,
                    end_ms: end_ms.max(p.first_ms),
                    start_exact: self.exact,
                    cause: self.cause,
                    count: p.count.max(1),
                },
                &p.pin,
            )
        })
    }

    /// Forget the tally after its gap row committed (its pin is released).
    pub(crate) fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
