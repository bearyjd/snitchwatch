//! Opening the history database for reading, and refusing what is not it.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use super::reader::{Unavail, UnavailKind};
use super::reader_sql::{scalar_i64, Control, Stop};
use super::schema::{schema_sql, SCHEMA_VERSION};
use crate::sqlite_file::{
    open_owner_only_readonly, require_known_schema, schema_object_count, OpenError,
};

fn no_retry(reason: impl Into<String>) -> Unavail {
    Unavail {
        reason: reason.into(),
        retry: false,
        kind: UnavailKind::Other,
    }
}

pub(super) fn from_stop(stop: Stop) -> Unavail {
    match stop {
        Stop::Busy => Unavail {
            reason: "history is busy: a write is in progress".into(),
            retry: true,
            kind: UnavailKind::Other,
        },
        Stop::Fail { reason, retry } => Unavail {
            reason: format!("history database: {reason}"),
            retry,
            kind: UnavailKind::Other,
        },
        Stop::Cancelled | Stop::Statement | Stop::Deadline => Unavail {
            reason: "history read was interrupted".into(),
            retry: true,
            kind: UnavailKind::Other,
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

/// What a failed schema check means. A lock is the writer's, and passes; only
/// a database that is not the history one, or is damaged, is refused for good.
pub(super) fn schema_unavailable(e: OpenError) -> Unavail {
    match e {
        OpenError::Sqlite(e) => from_stop(super::reader_sql::classify_error(&e)),
        other => no_retry(format!("history schema refused: {other}")),
    }
}

/// A read-only connection to the history database at `path`, whose
/// directory is the bridge's own 0700 one and whose schema is exactly
/// version 4's: the same object names and `CREATE` text a store creates, and
/// all of them (the check alone accepts a subset).
pub(super) fn open_validated(
    path: &Path,
    ctl: &Control,
    busy_wait: Duration,
) -> Result<Connection, Unavail> {
    if let Some(dir) = path.parent() {
        super::open::check_dir(dir, super::open::effective_uid())
            .map_err(|e| no_retry(e.to_string()))?;
    }
    let mut conn = open_owner_only_readonly(path).map_err(open_error)?;
    // A writer's commit window is short: wait it out, briefly, per statement.
    conn.busy_timeout(busy_wait)
        .map_err(|e| from_stop(super::reader_sql::classify_error(&e)))?;
    // The handler goes in before the first statement: a file that is not
    // ours can have a catalog of any size, and its check must not outlast
    // the budget (the writer's commit gives up after 250 ms) or a cancel.
    ctl.install(&mut conn)
        .map_err(|e| from_stop(super::reader_sql::classify_error(&e)))?;
    ctl.validating();
    let checked = check_schema(&conn, ctl);
    ctl.paging();
    checked?;
    Ok(conn)
}

/// A schema check that ran out of budget is not a lock to wait out: the same
/// file costs the same next time. Only a writer's lock is worth a retry.
fn validation_stop(stop: Stop) -> Unavail {
    match stop {
        Stop::Statement | Stop::Deadline => Unavail {
            reason: "history schema is too large to check within the read budget".into(),
            retry: false,
            kind: UnavailKind::TooExpensive,
        },
        other => from_stop(other),
    }
}

fn check_schema(conn: &Connection, ctl: &Control) -> Result<(), Unavail> {
    let version = scalar_i64(conn, ctl, "PRAGMA user_version").map_err(validation_stop)?;
    if version != SCHEMA_VERSION {
        return Err(no_retry(format!(
            "history schema version {version} is not the {SCHEMA_VERSION} this bridge reads"
        )));
    }
    let schema = schema_sql();
    ctl.begin_statement();
    require_known_schema(conn, &schema).map_err(|e| match e {
        OpenError::Sqlite(e) => validation_stop(ctl.stop_for(&e)),
        other => schema_unavailable(other),
    })?;
    let objects =
        scalar_i64(conn, ctl, "SELECT count(*) FROM sqlite_master").map_err(validation_stop)?;
    let expected = schema_object_count(&schema).map_err(schema_unavailable)?;
    if objects != expected {
        return Err(no_retry("the history schema is incomplete"));
    }
    Ok(())
}
