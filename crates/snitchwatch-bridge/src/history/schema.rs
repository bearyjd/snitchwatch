//! The history database's schema, pinned layout and version policy.
//!
//! **Layout** (set when the file is created, read back on every open; a
//! mismatch makes history `unavailable`, since changing either later needs a
//! full `VACUUM`, which S4 forbids): `page_size = 4096`,
//! `auto_vacuum = INCREMENTAL`. Per connection: `journal_mode = DELETE` (S2
//! and S3 reason about a rollback journal) and `secure_delete = ON` (S5).
//! None of the pragmas is part of [`schema_sql`], which is what
//! `require_known_schema` compares.
//!
//! **Version**: `PRAGMA user_version`. `require_known_schema` checks schema
//! objects, not version numbers, so the version is checked here: newer than
//! this build is refused; 0 with no objects (a new file) is created at the
//! current version, in one transaction with its tables.
//!
//! **Shape, with S2 in mind.** Every b-tree is a term in the per-row page
//! sum PR 2 must prove, so the schema has as few as it can: rowid tables,
//! and only fixed-width columns are indexed (`ts_ms`, the 16-byte
//! `rule_hash`). Nothing indexes `process_path` or `dst_host`. The `gaps` and
//! `sessions` tables of the design arrive with the ledger (PR 3), by a
//! version-2 migration; `meta` is not needed (the version is
//! `user_version`, the consent record is in the settings file).

use std::path::Path;

use rusqlite::Connection;

use super::caps::{
    DST_HOST, IP_TEXT, PROCESS_PATH, PROTOCOL, RULE_CLASS, RULE_HASH, RULE_NAME, SNAPSHOT,
};
use super::record::{ACTIONS, ANSWERED_BY};
use super::Unavailable;

pub const PAGE_SIZE: u32 = 4096;
/// `PRAGMA auto_vacuum` value for INCREMENTAL.
pub const AUTO_VACUUM_INCREMENTAL: i64 = 2;
pub const SCHEMA_VERSION: i64 = 1;

fn text_cap(column: &str, cap: usize) -> String {
    format!("CHECK ({column} IS NULL OR length(CAST({column} AS BLOB)) <= {cap})")
}

fn one_of(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
    quoted.join(", ")
}

/// The `CREATE` statements of [`SCHEMA_VERSION`]. Every cap of `caps.rs`
/// appears here as a `CHECK`, from the same constants, so the schema is the
/// proof that no value can exceed it. Generated once and byte-stable, since
/// `sqlite_master` keeps the text and `require_known_schema` compares it.
pub fn schema_sql() -> String {
    format!(
        "CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    ts_ms INTEGER NOT NULL,
    decided_ts_ms INTEGER,
    process_path TEXT {process_path},
    dst_host TEXT {dst_host},
    dst_ip TEXT {dst_ip},
    dst_port INTEGER CHECK (dst_port IS NULL OR dst_port BETWEEN 0 AND 65535),
    protocol TEXT {protocol},
    action TEXT NOT NULL CHECK (action IN ({actions})),
    answered_by TEXT NOT NULL CHECK (answered_by IN ({answered_by})),
    rule_name TEXT {rule_name},
    rule_hash BLOB CHECK (rule_hash IS NULL OR length(rule_hash) = {hash}),
    rule_class TEXT {rule_class},
    repeat_count INTEGER NOT NULL DEFAULT 1 CHECK (repeat_count BETWEEN 1 AND 4294967295),
    src_port INTEGER CHECK (src_port IS NULL OR src_port BETWEEN 0 AND 65535),
    src_ip TEXT {src_ip}
);
CREATE INDEX events_ts ON events (ts_ms);
CREATE INDEX events_rule_hash ON events (rule_hash);
CREATE TABLE rules_seen (
    id INTEGER PRIMARY KEY,
    rule_hash BLOB NOT NULL UNIQUE CHECK (length(rule_hash) = {hash}),
    snapshot TEXT NOT NULL CHECK (length(CAST(snapshot AS BLOB)) <= {snapshot}),
    first_seen_ms INTEGER NOT NULL
);
",
        process_path = text_cap("process_path", PROCESS_PATH),
        dst_host = text_cap("dst_host", DST_HOST),
        dst_ip = text_cap("dst_ip", IP_TEXT),
        src_ip = text_cap("src_ip", IP_TEXT),
        protocol = text_cap("protocol", PROTOCOL),
        rule_name = text_cap("rule_name", RULE_NAME),
        rule_class = text_cap("rule_class", RULE_CLASS),
        actions = one_of(&ACTIONS),
        answered_by = one_of(&ANSWERED_BY),
        hash = RULE_HASH,
        snapshot = SNAPSHOT,
    )
}

/// `sqlite_master` rows a complete v1 database has.
const OBJECTS: i64 = 5;

fn refuse(e: impl std::fmt::Display) -> Unavailable {
    Unavailable::new(format!("history database: {e}"))
}

fn int(conn: &Connection, pragma: &str) -> Result<i64, Unavailable> {
    conn.query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
        .map_err(refuse)
}

/// Open (creating a new file) and verify `path`; the connection comes back
/// with the per-connection pragmas set.
pub(crate) fn prepare(path: &Path) -> Result<Connection, Unavailable> {
    let conn = crate::sqlite_file::open_owner_only(path).map_err(refuse)?;
    let version = int(&conn, "user_version")?;
    if version > SCHEMA_VERSION {
        return Err(Unavailable::new(format!(
            "newer schema (version {version}; this bridge knows {SCHEMA_VERSION})"
        )));
    }
    let objects: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
        .map_err(refuse)?;
    let schema = schema_sql();
    match version {
        0 if objects == 0 => create(&conn, &schema)?,
        0 => return Err(refuse("not a Snitchwatch history database")),
        SCHEMA_VERSION => {
            crate::sqlite_file::require_known_schema(&conn, &schema).map_err(refuse)?;
            if objects != OBJECTS {
                return Err(refuse("the schema is incomplete"));
            }
        }
        other => return Err(refuse(format!("unknown schema version {other}"))),
    }
    let page_size = int(&conn, "page_size")?;
    if page_size != i64::from(PAGE_SIZE) {
        return Err(refuse(format!("page size is {page_size}, not {PAGE_SIZE}")));
    }
    let auto_vacuum = int(&conn, "auto_vacuum")?;
    if auto_vacuum != AUTO_VACUUM_INCREMENTAL {
        return Err(refuse("auto_vacuum is not INCREMENTAL"));
    }
    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(refuse)?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(refuse(format!("journal mode is {mode}, not delete")));
    }
    conn.execute_batch("PRAGMA secure_delete = ON;")
        .map_err(refuse)?;
    if int(&conn, "secure_delete")? != 1 {
        return Err(refuse("secure_delete could not be enabled"));
    }
    Ok(conn)
}

/// A new file: the layout pragmas (they take effect only before the first
/// table exists), then the tables and the version, in one transaction.
fn create(conn: &Connection, schema: &str) -> Result<(), Unavailable> {
    conn.execute_batch(&format!(
        "PRAGMA page_size = {PAGE_SIZE}; PRAGMA auto_vacuum = {AUTO_VACUUM_INCREMENTAL};"
    ))
    .map_err(refuse)?;
    conn.execute_batch(&format!(
        "BEGIN; {schema} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
    ))
    .map_err(refuse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_is_byte_stable() {
        assert_eq!(schema_sql(), schema_sql());
    }

    #[test]
    fn a_complete_database_has_the_expected_object_count() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&schema_sql()).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        // 2 tables + 2 indexes + the UNIQUE index on rules_seen.rule_hash.
        assert_eq!(n, OBJECTS);
    }
}
