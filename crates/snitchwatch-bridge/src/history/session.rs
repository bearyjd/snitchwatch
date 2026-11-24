//! Connection history PR 3b: the session ledger and the durable coverage
//! watermark (design D6).
//!
//! Every time recording starts, the new session is compared with the previous
//! one. A **clean stop** has an exact `end_ms`: the gap from there to the new
//! start is an exact `bridge-down` (`disabled` if history was switched off).
//! A **crash** has no end: the gap runs from `meta.watermark_ms`, which W1
//! keeps at or before every accepted-but-uncommitted item, so the start is
//! only a bound (`start_exact = false`).

use rusqlite::{params, Connection, OptionalExtension};

use super::ledger::{enforce_limits, record_gap, Gap, GapCause, LedgerLimits};
use super::schema::END_KINDS;

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// Stopped by the bridge, with nothing accepted left uncommitted.
    Clean,
    /// History was switched off.
    Disabled,
    /// No clean end (a crash, or a stop with uncommitted items): the end is
    /// only the watermark.
    Unclean,
}

impl SessionEnd {
    pub fn as_str(self) -> &'static str {
        END_KINDS[self as usize]
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Clean, Self::Disabled, Self::Unclean]
            .into_iter()
            .find(|k| k.as_str() == text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    /// `None` while the session is open (or crashed and not yet recovered).
    pub end: Option<SessionEnd>,
}

/// The durable coverage watermark; `None` before the first session.
pub fn read_watermark(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT watermark_ms FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })
    .optional()
}

/// Raises the watermark to `ms`. It never moves backwards: coverage already
/// claimed is not un-claimed by a clock that jumped.
pub fn write_watermark(conn: &Connection, ms: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (id, watermark_ms) VALUES (1, ?1) \
         ON CONFLICT (id) DO UPDATE SET watermark_ms = max(watermark_ms, excluded.watermark_ms)",
        [ms],
    )?;
    Ok(())
}

/// [`write_watermark`], then lowered to `ceiling` (the oldest live pin) if it
/// stands past it: a clock that stepped back can leave a pin older than a
/// watermark already written, and a crash then must not treat that pin's
/// stretch as covered. With no live pin the watermark still never moves back.
pub(crate) fn write_watermark_capped(
    conn: &Connection,
    ms: i64,
    ceiling: Option<i64>,
) -> rusqlite::Result<()> {
    write_watermark(conn, ms)?;
    if let Some(ceiling) = ceiling {
        conn.execute(
            "UPDATE meta SET watermark_ms = ?1 WHERE id = 1 AND watermark_ms > ?1",
            [ceiling],
        )?;
    }
    Ok(())
}

/// Every session, oldest first.
pub fn read_sessions(conn: &Connection) -> rusqlite::Result<Vec<Session>> {
    let mut stmt = conn.prepare("SELECT start_ms, end_ms, end_kind FROM sessions ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let kind: Option<String> = r.get(2)?;
        Ok(Session {
            start_ms: r.get(0)?,
            end_ms: r.get(1)?,
            end: kind.as_deref().and_then(SessionEnd::parse),
        })
    })?;
    rows.collect()
}

/// Starts a session at `now_ms`, closing the previous one into a gap row.
/// Returns that gap (`None` for the very first session). `watermark_ms` is the
/// watermark the transaction may write (W1): not past any live pin.
pub fn start_session(
    conn: &Connection,
    limits: &LedgerLimits,
    now_ms: i64,
    watermark_ms: i64,
) -> rusqlite::Result<Option<Gap>> {
    let gap = close_previous(conn, now_ms)?;
    if let Some(gap) = &gap {
        record_gap(conn, gap, limits)?;
    }
    conn.execute(
        "INSERT INTO sessions (start_ms) VALUES (?1)",
        params![now_ms],
    )?;
    write_watermark(conn, watermark_ms)?;
    enforce_limits(conn, limits)?;
    Ok(gap)
}

/// The gap between the newest session's end and `now_ms`. A session with no
/// end crashed: it is closed `unclean` at the watermark (exactly once), and
/// the gap starts there with only a bound. A clean end is exact; an unclean
/// one (a stop with uncommitted items) is not.
fn close_previous(conn: &Connection, now_ms: i64) -> rusqlite::Result<Option<Gap>> {
    let previous: Option<(i64, i64, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT id, start_ms, end_ms, end_kind FROM sessions ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, start_ms, end_ms, kind)) = previous else {
        return Ok(None);
    };
    let kind = kind.as_deref().and_then(SessionEnd::parse);
    let (from, exact, cause) = match (end_ms, kind) {
        (Some(end), Some(kind)) => (
            end,
            kind != SessionEnd::Unclean,
            if kind == SessionEnd::Disabled {
                GapCause::Disabled
            } else {
                GapCause::BridgeDown
            },
        ),
        _ => {
            let watermark = read_watermark(conn)?.unwrap_or(start_ms);
            let from = watermark.max(start_ms);
            conn.execute(
                "UPDATE sessions SET end_ms = ?1, end_kind = ?2 WHERE id = ?3",
                params![from, SessionEnd::Unclean.as_str(), id],
            )?;
            (from, false, GapCause::BridgeDown)
        }
    };
    Ok(Some(Gap {
        start_ms: from,
        end_ms: now_ms.max(from),
        start_exact: exact,
        cause,
        count: 1,
    }))
}

/// Ends the open session at `end_ms`.
pub fn end_session(conn: &Connection, end_ms: i64, kind: SessionEnd) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET end_ms = ?1, end_kind = ?2 WHERE end_kind IS NULL",
        params![end_ms, kind.as_str()],
    )?;
    Ok(())
}
