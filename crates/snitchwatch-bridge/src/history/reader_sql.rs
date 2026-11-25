//! The reader's statements and the controls that keep each one short.
//!
//! Every statement runs in autocommit and is finished (reset) before the
//! next, so **no read lock is held between statements** and the writer can
//! commit (journal mode DELETE: a reader's SHARED lock blocks the writer's
//! EXCLUSIVE one, and the writer gives up after 250 ms). A progress handler
//! interrupts a statement at the statement budget, at the query deadline, or
//! on cancel; rows already read are kept.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, Row};

use super::ledger::{Gap, GapCause};
use super::reader::{EventView, Limits};
use super::reader_registry::CancelToken;
use super::session::{Session, SessionEnd};

/// Progress-handler tick, in SQLite VM instructions.
const TICK_OPS: i32 = 1000;
/// More rows than the ledger's own ceiling (L1): never a reason to read on.
pub(super) const LEDGER_ROWS_MAX: usize = 4096;

const WHY_NONE: u8 = 0;
const WHY_CANCELLED: u8 = 1;
const WHY_STATEMENT: u8 = 2;
const WHY_DEADLINE: u8 = 3;

/// Why a statement or the query stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stop {
    Cancelled,
    /// The statement ran past its budget.
    Statement,
    /// The query ran past its deadline.
    Deadline,
    /// A writer's commit held the file past the short busy wait.
    Busy,
    /// Anything else: the file is not readable.
    Fail { reason: String, retry: bool },
}

pub(super) fn classify_error(e: &rusqlite::Error) -> Stop {
    let code = match e {
        rusqlite::Error::SqliteFailure(f, _) => f.code,
        _ => {
            return Stop::Fail {
                reason: e.to_string(),
                retry: false,
            }
        }
    };
    match code {
        ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Stop::Busy,
        // A hot journal a read-only connection cannot roll back, or a file
        // that vanished under us: worth another try.
        ErrorCode::ReadOnly | ErrorCode::CannotOpen => Stop::Fail {
            reason: e.to_string(),
            retry: true,
        },
        _ => Stop::Fail {
            reason: e.to_string(),
            retry: false,
        },
    }
}

/// Cancel, statement budget and deadline for one query.
pub(super) struct Control {
    token: CancelToken,
    base: Instant,
    statement_start_ns: Arc<AtomicU64>,
    why: Arc<AtomicU8>,
    budget: Duration,
    deadline: Duration,
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

impl Control {
    pub(super) fn new(token: &CancelToken, limits: &Limits) -> Self {
        Self {
            token: token.clone(),
            base: Instant::now(),
            statement_start_ns: Arc::new(AtomicU64::new(0)),
            why: Arc::new(AtomicU8::new(WHY_NONE)),
            budget: limits.statement_budget,
            deadline: limits.query_deadline,
        }
    }

    /// Interrupts a running statement at the budget, the deadline or a
    /// cancel, and records which.
    pub(super) fn install(&self, conn: &mut Connection) -> rusqlite::Result<()> {
        let token = self.token.clone();
        let base = self.base;
        let start = Arc::clone(&self.statement_start_ns);
        let why = Arc::clone(&self.why);
        let (budget, deadline) = (ns(self.budget), ns(self.deadline));
        conn.progress_handler(
            TICK_OPS,
            Some(move || {
                let now = ns(base.elapsed());
                let reason = if token.is_cancelled() {
                    WHY_CANCELLED
                } else if now.saturating_sub(start.load(Ordering::Relaxed)) > budget {
                    WHY_STATEMENT
                } else if now > deadline {
                    WHY_DEADLINE
                } else {
                    return false;
                };
                why.store(reason, Ordering::SeqCst);
                true
            }),
        )
    }

    pub(super) fn begin_statement(&self) {
        self.why.store(WHY_NONE, Ordering::SeqCst);
        self.statement_start_ns
            .store(ns(self.base.elapsed()), Ordering::Relaxed);
    }

    pub(super) fn cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub(super) fn deadline_passed(&self) -> bool {
        self.base.elapsed() >= self.deadline
    }

    /// What an error from a statement means.
    pub(super) fn stop_for(&self, e: &rusqlite::Error) -> Stop {
        let interrupted = matches!(
            e,
            rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::OperationInterrupted
        );
        if !interrupted {
            return classify_error(e);
        }
        match self.why.load(Ordering::SeqCst) {
            WHY_STATEMENT => Stop::Statement,
            WHY_DEADLINE => Stop::Deadline,
            _ => Stop::Cancelled,
        }
    }
}

/// Lossy text: a crafted file with invalid UTF-8 must not break a read.
fn text(row: &Row<'_>, i: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(i)? {
        ValueRef::Null => None,
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        ValueRef::Integer(n) => Some(n.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
    })
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<EventView> {
    Ok(EventView {
        id: row.get(0)?,
        ts_ms: row.get(1)?,
        decided_ts_ms: row.get(2)?,
        process_path: text(row, 3)?,
        dst_host: text(row, 4)?,
        dst_ip: text(row, 5)?,
        dst_port: row.get(6)?,
        protocol: text(row, 7)?,
        action: text(row, 8)?.unwrap_or_default(),
        answered_by: text(row, 9)?.unwrap_or_default(),
        rule_name: text(row, 10)?,
        rule_hash: text(row, 11)?,
        rule_class: text(row, 12)?,
        repeat_count: row.get(13)?,
        src_port: row.get(14)?,
        src_ip: text(row, 15)?,
        prompt_outcome: text(row, 16)?,
        possible_duplicate: row.get::<_, i64>(17)? != 0,
    })
}

const EVENT_COLUMNS: &str = "id, ts_ms, decided_ts_ms, process_path, dst_host, dst_ip, \
    dst_port, protocol, action, answered_by, rule_name, lower(hex(rule_hash)), rule_class, \
    repeat_count, src_port, src_ip, prompt_outcome, possible_duplicate";

/// One statement's rows and why it ended early, if it did.
pub(super) struct Chunk {
    pub rows: Vec<EventView>,
    pub stop: Option<Stop>,
}

/// Up to `n` events older than `after` (newest first), as one statement.
/// The seek is a row-value comparison over `(ts_ms, id)`: keyset, never
/// `OFFSET`, and index-ordered on `events_ts`.
pub(super) fn events_chunk(
    conn: &Connection,
    ctl: &Control,
    after: Option<(i64, i64)>,
    n: usize,
) -> Chunk {
    let sql = match after {
        Some(_) => format!(
            "SELECT {EVENT_COLUMNS} FROM history_events WHERE (ts_ms, id) < (?1, ?2) \
             ORDER BY ts_ms DESC, id DESC LIMIT ?3"
        ),
        None => format!(
            "SELECT {EVENT_COLUMNS} FROM history_events \
             ORDER BY ts_ms DESC, id DESC LIMIT ?1"
        ),
    };
    let mut chunk = Chunk {
        rows: Vec::new(),
        stop: None,
    };
    let limit = i64::try_from(n).unwrap_or(i64::MAX);
    let (ts, id) = after.unwrap_or((0, 0));
    let run = (|| -> Result<(), rusqlite::Error> {
        let mut stmt = conn.prepare(&sql)?;
        ctl.begin_statement();
        let mut rows = if after.is_some() {
            stmt.query(rusqlite::params![ts, id, limit])?
        } else {
            stmt.query(rusqlite::params![limit])?
        };
        while let Some(row) = rows.next()? {
            chunk.rows.push(event_from_row(row)?);
        }
        Ok(())
    })();
    if let Err(e) = run {
        chunk.stop = Some(ctl.stop_for(&e));
    }
    chunk
}

fn finish_statement<T>(
    conn: &Connection,
    ctl: &Control,
    result: rusqlite::Result<T>,
) -> Result<T, Stop> {
    let value = result.map_err(|e| ctl.stop_for(&e))?;
    // No transaction may outlive a statement: the lock would block a commit.
    if conn.is_autocommit() {
        Ok(value)
    } else {
        Err(Stop::Fail {
            reason: "a read transaction was left open".into(),
            retry: false,
        })
    }
}

fn run<T>(
    conn: &Connection,
    ctl: &Control,
    body: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T, Stop> {
    if ctl.cancelled() {
        return Err(Stop::Cancelled);
    }
    ctl.begin_statement();
    finish_statement(conn, ctl, body())
}

/// The earliest surviving event: the retention floor.
pub(super) fn floor(conn: &Connection, ctl: &Control) -> Result<Option<i64>, Stop> {
    run(conn, ctl, || {
        conn.query_row("SELECT min(ts_ms) FROM events", [], |r| r.get(0))
    })
}

/// The earliest start of any ledger row.
pub(super) fn ledger_start(conn: &Connection, ctl: &Control) -> Result<Option<i64>, Stop> {
    run(conn, ctl, || {
        conn.query_row(
            "SELECT min(m) FROM (SELECT min(start_ms) AS m FROM gaps \
             UNION ALL SELECT min(start_ms) FROM sessions)",
            [],
            |r| r.get(0),
        )
    })
}

pub(super) fn watermark(conn: &Connection, ctl: &Control) -> Result<Option<i64>, Stop> {
    run(conn, ctl, || super::session::read_watermark(conn))
}

/// Gap rows overlapping `[lo, hi]`, newest start first. `gaps` has no index
/// on its bounds: this scans it, bounded by the ledger ceiling (L1) and the
/// statement budget.
pub(super) fn gaps_overlapping(
    conn: &Connection,
    ctl: &Control,
    lo: i64,
    hi: i64,
) -> Result<Vec<Gap>, Stop> {
    run(conn, ctl, || {
        let mut stmt = conn.prepare(
            "SELECT start_ms, end_ms, start_exact, cause, count FROM gaps \
             WHERE end_ms >= ?1 AND start_ms <= ?2 ORDER BY start_ms DESC, id DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(LEDGER_ROWS_MAX).unwrap_or(i64::MAX);
        let rows = stmt.query_map(rusqlite::params![lo, hi, limit], |r| {
            let cause: String = r.get(3)?;
            let count: i64 = r.get(4)?;
            Ok(Gap {
                start_ms: r.get(0)?,
                end_ms: r.get(1)?,
                start_exact: r.get::<_, i64>(2)? != 0,
                // The schema's CHECK keeps this to the known causes.
                cause: GapCause::parse(&cause).unwrap_or(GapCause::Uncertain),
                count: u64::try_from(count).unwrap_or(1),
            })
        })?;
        rows.collect()
    })
}

fn end_kind(text: Option<&str>) -> Option<SessionEnd> {
    [SessionEnd::Clean, SessionEnd::Disabled, SessionEnd::Unclean]
        .into_iter()
        .find(|k| Some(k.as_str()) == text)
}

/// Session rows overlapping `[lo, hi]`, newest first. An open session
/// (`end_ms` NULL) always overlaps what follows its start.
pub(super) fn sessions_overlapping(
    conn: &Connection,
    ctl: &Control,
    lo: i64,
    hi: i64,
) -> Result<Vec<Session>, Stop> {
    run(conn, ctl, || {
        let mut stmt = conn.prepare(
            "SELECT start_ms, end_ms, end_kind FROM sessions \
             WHERE start_ms <= ?2 AND (end_ms IS NULL OR end_ms >= ?1) \
             ORDER BY start_ms DESC, id DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(LEDGER_ROWS_MAX).unwrap_or(i64::MAX);
        let rows = stmt.query_map(rusqlite::params![lo, hi, limit], |r| {
            let kind: Option<String> = r.get(2)?;
            Ok(Session {
                start_ms: r.get(0)?,
                end_ms: r.get(1)?,
                end: end_kind(kind.as_deref()),
            })
        })?;
        rows.collect()
    })
}

/// A statement that is not rows: the schema checks of [`super::reader_open`].
pub(super) fn scalar_i64(conn: &Connection, ctl: &Control, sql: &str) -> Result<i64, Stop> {
    run(conn, ctl, || conn.query_row(sql, [], |r| r.get(0)))
}
