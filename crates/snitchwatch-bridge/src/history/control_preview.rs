//! PR 10: preview. Counts what a clear or delete would remove, changes no row
//! nothing, and hands back a token the gate remembers (at most
//! [`MAX_TOKENS`] live, each expiring) so that a confirm can only name a
//! preview this process issued, under the generation it was issued in.
//!
//! The scan is advisory and bounded the way the reader is: windows of ids,
//! each statement cut at 100 ms in autocommit, no lock held between
//! statements, on a read-only connection that holds a **reader slot** (so a
//! control operation's quiesce cancels it). The authoritative checksum is
//! recomputed at confirm, with the writer stopped.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::control::{ControlGate, Refused};
use super::reader_registry::{NotAdmitted, ReaderTicket};

/// Live preview tokens at once; the oldest is dropped for a new one.
pub const MAX_TOKENS: usize = 4;
/// Ids per statement window.
const CHUNK: i64 = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Events that a clear would remove (repeats counted).
    pub count: u64,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    /// An estimate: the size of the store file.
    pub approx_bytes: u64,
    /// A tombstone of a removal that did not finish is still on disk: its
    /// rows are counted above (unless it could not be scanned, then the
    /// count is short) and a clear or delete removes it.
    pub leftover: bool,
    pub token: TokenId,
}

/// What the previewed rows add up to; compared again at confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Checksum {
    pub rows: i64,
    pub id_sum: i64,
    pub repeat_sum: i64,
}

pub(super) struct Scan {
    pub max_id: i64,
    pub checksum: Checksum,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

/// What a preview saw of the tombstone, checked again at confirm before the
/// tombstone is unlinked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leftover {
    /// None was there: one that appears since is a change.
    Absent,
    /// One was there that could not be scanned: finishing it needs no
    /// recheck, so a retry can still start.
    Unchecked,
    /// One was there and these rows were counted.
    Counted(Checksum),
}

pub(super) struct Token {
    pub generation: u64,
    pub max_id: i64,
    pub checksum: Checksum,
    pub leftover: Leftover,
}

struct Entry {
    id: TokenId,
    issued: Instant,
    token: Token,
}

#[derive(Default)]
pub(super) struct Tokens(Mutex<Vec<Entry>>);

impl Tokens {
    pub(super) fn issue(&self, token: Token) -> TokenId {
        let id = TokenId(rand::random());
        let mut live = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if live.len() >= MAX_TOKENS {
            live.remove(0);
        }
        live.push(Entry {
            id,
            issued: Instant::now(),
            token,
        });
        id
    }

    /// Spends `id`: found, not older than `ttl`, from `generation`.
    pub(super) fn take(&self, id: TokenId, generation: u64, ttl: Duration) -> Option<Token> {
        let mut live = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let at = live.iter().position(|e| e.id == id)?;
        let entry = live.remove(at);
        (entry.issued.elapsed() <= ttl && entry.token.generation == generation)
            .then_some(entry.token)
    }
}

/// Asked between statements and from the progress handler.
pub(super) type Cancelled = Arc<dyn Fn() -> bool + Send + Sync>;

/// Why a scan gave no answer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ScanError {
    Cancelled,
    Failed(String),
}

fn sql(e: rusqlite::Error) -> ScanError {
    ScanError::Failed(e.to_string())
}

/// Scans `events` with `id <= upto` (the current max when `None`), window by
/// window. Each statement is cut after `budget` (never, when `None`) or on
/// `cancelled`.
pub(super) fn scan_events(
    conn: &mut Connection,
    upto: Option<i64>,
    cancelled: Cancelled,
    budget: Option<Duration>,
) -> Result<Scan, ScanError> {
    let window = Arc::new(Mutex::new(Instant::now()));
    let (flag, started) = (cancelled.clone(), window.clone());
    conn.progress_handler(
        1000,
        Some(move || {
            flag()
                || started
                    .lock()
                    .map_or(true, |at| budget.is_some_and(|b| at.elapsed() > b))
        }),
    )
    .map_err(sql)?;
    let max_id = match upto {
        Some(max) => max,
        None => conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
            .map_err(sql)?,
    };
    let mut scan = Scan {
        max_id,
        checksum: Checksum::default(),
        from_ms: None,
        to_ms: None,
    };
    let mut low = 0;
    while low < max_id {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        *window.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        let high = (low + CHUNK).min(max_id);
        add_window(conn, &mut scan, low, high)?;
        low = high;
    }
    conn.progress_handler(0, None::<fn() -> bool>)
        .map_err(sql)?;
    Ok(scan)
}

fn add_window(conn: &Connection, scan: &mut Scan, low: i64, high: i64) -> Result<(), ScanError> {
    let (rows, ids, repeats, from, to): (i64, i64, i64, Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(id), 0), COALESCE(SUM(repeat_count), 0), \
             MIN(ts_ms), MAX(ts_ms) FROM events WHERE id > ?1 AND id <= ?2",
            [low, high],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|e| {
            if e.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
                ScanError::Cancelled
            } else {
                sql(e)
            }
        })?;
    let sum = &mut scan.checksum;
    sum.rows += rows;
    sum.id_sum += ids;
    sum.repeat_sum += repeats;
    scan.from_ms = [scan.from_ms, from].into_iter().flatten().min();
    scan.to_ms = [scan.to_ms, to].into_iter().flatten().max();
    Ok(())
}

fn empty_scan() -> Scan {
    Scan {
        max_id: 0,
        checksum: Checksum::default(),
        from_ms: None,
        to_ms: None,
    }
}

/// At confirm, before the tombstone is unlinked: it still holds the rows the
/// preview counted. Runs with the readers quiesced, so it is not bounded.
pub(super) fn recheck_tombstone(location: &super::Location, token: &Token) -> Result<(), Refused> {
    let path = location.dir().join(super::control_files::TOMBSTONE);
    let present = path.symlink_metadata().is_ok();
    let want = match (token.leftover, present) {
        (Leftover::Unchecked, _) | (Leftover::Absent, false) => return Ok(()),
        (Leftover::Absent, true) => return Err(Refused::Changed),
        (Leftover::Counted(want), _) => want,
    };
    let have = if present {
        let mut conn = crate::sqlite_file::open_owner_only_readonly(&path)
            .map_err(|e| Refused::Unavailable(e.to_string()))?;
        scan_events(&mut conn, None, Arc::new(|| false), None)
            .map_err(|e| match e {
                ScanError::Cancelled => Refused::Busy,
                ScanError::Failed(reason) => Refused::Unavailable(reason),
            })?
            .checksum
    } else {
        Checksum::default()
    };
    if have == want {
        Ok(())
    } else {
        Err(Refused::Changed)
    }
}

impl ControlGate {
    /// Scans one file with a reader slot's cancel token; a missing file is
    /// an empty scan.
    fn scan_file(&self, ticket: &ReaderTicket, path: &Path) -> Result<Scan, Refused> {
        match crate::sqlite_file::open_owner_only_readonly(path) {
            Ok(mut conn) => {
                let _ = conn.busy_timeout(Duration::from_millis(20));
                let cancel = ticket.cancel_token();
                cancel.attach(conn.get_interrupt_handle());
                let seen = cancel.clone();
                scan_events(
                    &mut conn,
                    None,
                    Arc::new(move || seen.is_cancelled()),
                    Some(self.0.limits.statement),
                )
                .map_err(|e| match e {
                    ScanError::Cancelled => Refused::Busy,
                    ScanError::Failed(reason) => Refused::Unavailable(reason),
                })
            }
            Err(crate::sqlite_file::OpenError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(empty_scan())
            }
            Err(e) => Err(Refused::Unavailable(e.to_string())),
        }
    }

    /// The tombstone's scan, if there is one. With no store beside it, one
    /// that cannot be scanned (a second link, a failure, a timeout) is
    /// reported unchecked and uncounted rather than refused: the delete that
    /// `DataLeft` says to retry must be able to start. Beside a store the
    /// preview is refused, so no clear unlinks rows it never counted.
    fn scan_tombstone(
        &self,
        ticket: &ReaderTicket,
        location: &super::Location,
        store_present: bool,
    ) -> Result<(Scan, Leftover), Refused> {
        let path = location.dir().join(super::control_files::TOMBSTONE);
        if path.symlink_metadata().is_err() {
            return Ok((empty_scan(), Leftover::Absent));
        }
        match self.scan_file(ticket, &path) {
            Ok(scan) => {
                let counted = Leftover::Counted(scan.checksum);
                Ok((scan, counted))
            }
            Err(Refused::Busy) if ticket.cancel_token().is_cancelled() => Err(Refused::Busy),
            Err(Refused::Busy | Refused::Unavailable(_)) if !store_present => {
                Ok((empty_scan(), Leftover::Unchecked))
            }
            Err(Refused::Busy | Refused::Unavailable(_)) => Err(Refused::Unavailable(
                "the leftover of an old store could not be checked; restart the bridge \
                 to finish removing it"
                    .into(),
            )),
            Err(other) => Err(other),
        }
    }

    /// Counts what a clear would remove: the store and a tombstone beside
    /// it. Takes a reader slot while it scans, so a control operation sees
    /// it (and cancels it); changes no row, pins nothing and numbers
    /// nothing. It may run [`recover`](super::control_files::recover) and
    /// resume recording, as any first use of the gate does (the writer lock
    /// is taken lazily).
    pub fn preview(&self) -> Result<Preview, Refused> {
        let location = &self.owned_location()?;
        let ticket = self.0.registry.try_acquire().map_err(|e| match e {
            NotAdmitted::Busy => Refused::Busy,
            NotAdmitted::ControlOperation => Refused::InProgress,
        })?;
        let generation = self.0.generation.current();
        let db = location.db_path();
        let tomb = location.dir().join(super::control_files::TOMBSTONE);
        let approx_bytes = [&db, &tomb]
            .iter()
            .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
            .sum();
        let store = self.scan_file(&ticket, &db)?;
        let (left, leftover) =
            self.scan_tombstone(&ticket, location, db.symlink_metadata().is_ok())?;
        let rows = |s: &Scan| u64::try_from(s.checksum.repeat_sum).unwrap_or(0);
        let count = rows(&store) + rows(&left);
        let token = self.0.tokens.issue(Token {
            generation,
            max_id: store.max_id,
            checksum: store.checksum,
            leftover,
        });
        Ok(Preview {
            count,
            from_ms: [store.from_ms, left.from_ms].into_iter().flatten().min(),
            to_ms: [store.to_ms, left.to_ms].into_iter().flatten().max(),
            approx_bytes,
            leftover: leftover != Leftover::Absent,
            token,
        })
    }
}
