//! Opening the history database for reading, and refusing what is not it.

use std::path::Path;

use rusqlite::Connection;

use super::reader::Unavail;
use super::reader_sql::{scalar_i64, Control, Stop};
use super::schema::{schema_sql, SCHEMA_VERSION};
use crate::sqlite_file::{open_owner_only_readonly, require_known_schema, OpenError};

fn no_retry(reason: impl Into<String>) -> Unavail {
    Unavail {
        reason: reason.into(),
        retry: false,
    }
}

pub(super) fn from_stop(stop: Stop) -> Unavail {
    match stop {
        Stop::Busy => Unavail {
            reason: "history is busy: a write is in progress".into(),
            retry: true,
        },
        Stop::Fail { reason, retry } => Unavail {
            reason: format!("history database: {reason}"),
            retry,
        },
        Stop::Cancelled | Stop::Statement | Stop::Deadline => Unavail {
            reason: "history read was interrupted".into(),
            retry: true,
        },
    }
}

fn open_error(e: OpenError) -> Unavail {
    match e {
        OpenError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => no_retry("no history"),
        OpenError::Io(io) => no_retry(format!("history database: {io}")),
        OpenError::Refused(why) => no_retry(format!("history database: {why}")),
        OpenError::Sqlite(e) => from_stop(super::reader_sql::classify_error(&e)),
    }
}

/// A read-only connection to the history database at `path`, whose
/// directory is the bridge's own 0700 one and whose schema is exactly
/// version 4's: the same object names and `CREATE` text a store creates, and
/// all of them (the check alone accepts a subset).
pub(super) fn open_validated(path: &Path, ctl: &Control) -> Result<Connection, Unavail> {
    if let Some(dir) = path.parent() {
        super::open::check_dir(dir, super::open::effective_uid())
            .map_err(|e| no_retry(e.to_string()))?;
    }
    let mut conn = open_owner_only_readonly(path).map_err(open_error)?;
    // The schema check reads a handful of rows; the budget applies to the
    // page statements that follow.
    check_schema(&conn, ctl)?;
    ctl.install(&mut conn)
        .map_err(|e| from_stop(super::reader_sql::classify_error(&e)))?;
    Ok(conn)
}

fn check_schema(conn: &Connection, ctl: &Control) -> Result<(), Unavail> {
    let version = scalar_i64(conn, ctl, "PRAGMA user_version").map_err(from_stop)?;
    if version != SCHEMA_VERSION {
        return Err(no_retry(format!(
            "history schema version {version} is not the {SCHEMA_VERSION} this bridge reads"
        )));
    }
    let schema = schema_sql();
    require_known_schema(conn, &schema)
        .map_err(|e| no_retry(format!("history schema refused: {e}")))?;
    let objects = scalar_i64(conn, ctl, "SELECT count(*) FROM sqlite_master").map_err(from_stop)?;
    let reference = Connection::open_in_memory().map_err(|e| no_retry(e.to_string()))?;
    reference
        .execute_batch(&schema)
        .map_err(|e| no_retry(e.to_string()))?;
    let expected: i64 = reference
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .map_err(|e| no_retry(e.to_string()))?;
    if objects != expected {
        return Err(no_retry("the history schema is incomplete"));
    }
    Ok(())
}
