//! The history database's schema, pinned layout and version policy.
//!
//! **Layout** (set when the file is created, read back on every open; a
//! mismatch makes history `unavailable`, since changing either later needs a
//! full `VACUUM`, which S4 forbids): `page_size = 4096`,
//! `auto_vacuum = INCREMENTAL`. Per connection: `journal_mode = DELETE` (S2
//! and S3 reserve room for a full rollback journal) and `secure_delete = ON` (S5).
//! None of the pragmas is part of [`schema_sql`], which is what
//! `require_known_schema` compares.
//!
//! **Version**: `PRAGMA user_version`. `require_known_schema` checks schema
//! objects, not version numbers, so the version is checked here: newer than
//! this build is refused; 0 with no objects (a new file) is created at the
//! current version, in one transaction with its tables.
//!
//! **Shape.** Fewer b-trees mean less write amplification, so the schema has
//! as few as it can: rowid tables, and only fixed-width columns are indexed
//! (`ts_ms`, the 16-byte `rule_hash`). Nothing indexes `process_path` or `dst_host`. The `gaps` and
//! `sessions` tables of the design (D6) and the one-row `meta` table holding
//! the coverage watermark arrive in version 2 ([`v2_additions_sql`]); the
//! consent record is in the settings file.
//!
//! **Migration** (v1 to v2): the v1 text ([`v1_sql`]) is frozen byte for byte,
//! since `sqlite_master` keeps it and `require_known_schema` compares it. An
//! existing v1 file is checked against it, then the additions and
//! `user_version = 2` go in one transaction, so a failure leaves a v1 file.
//! Only `open_store` reaches this, and it refuses unless history is enabled:
//! with history off nothing is opened, created or migrated.

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
pub const SCHEMA_VERSION: i64 = 2;

fn text_cap(column: &str, cap: usize) -> String {
    format!("CHECK ({column} IS NULL OR length(CAST({column} AS BLOB)) <= {cap})")
}

fn one_of(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
    quoted.join(", ")
}

/// Both tables are `STRICT`: with plain column affinity a TEXT value in a BLOB
/// column is kept as TEXT, and `length()` stops at a NUL, so a "16-byte" hash
/// could be a megabyte. Strict typing plus the `CHECK`s below is what makes
/// the S1 caps hold.
///
/// The `CREATE` statements of schema version 1, frozen. Every cap of
/// `caps.rs` appears here as a `CHECK`, from the same constants, so the schema
/// is the proof that no value can exceed it. Byte-stable, since
/// `sqlite_master` keeps the text and `require_known_schema` compares it.
pub fn v1_sql() -> String {
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
) STRICT;
CREATE INDEX events_ts ON events (ts_ms);
CREATE INDEX events_rule_hash ON events (rule_hash);
CREATE TABLE rules_seen (
    id INTEGER PRIMARY KEY,
    rule_hash BLOB NOT NULL UNIQUE CHECK (length(rule_hash) = {hash}),
    snapshot TEXT NOT NULL CHECK (length(CAST(snapshot AS BLOB)) <= {snapshot}),
    first_seen_ms INTEGER NOT NULL
) STRICT;
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

/// `gaps.cause`, as stored (the design's D6 table; the enum that names them is
/// `ledger::GapCause`, in the same order).
pub(crate) const GAP_CAUSES: [&str; 10] = [
    "bridge-down",
    "daemon-disconnected",
    "upstream-loss",
    "queue-overflow",
    "admission-drop",
    "write-failed",
    "disabled",
    "evicted",
    "clock-jump",
    "uncertain",
];
/// `sessions.end_kind`, as stored, in the order of `session::SessionEnd`.
pub(crate) const END_KINDS: [&str; 3] = ["clean", "disabled", "unclean"];

/// What version 2 adds (D6, L1): the gap ledger, the session ledger and the
/// coverage watermark. Every column is fixed-width or a short enum, so a row's
/// size is bounded (`ledger::ROW_BYTES`).
pub fn v2_additions_sql() -> String {
    format!(
        "CREATE TABLE gaps (
    id INTEGER PRIMARY KEY,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL CHECK (end_ms >= start_ms),
    start_exact INTEGER NOT NULL CHECK (start_exact IN (0, 1)),
    cause TEXT NOT NULL CHECK (cause IN ({causes})),
    count INTEGER NOT NULL CHECK (count >= 1)
) STRICT;
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER,
    end_kind TEXT CHECK (end_kind IS NULL OR end_kind IN ({ends})),
    CHECK ((end_ms IS NULL) = (end_kind IS NULL))
) STRICT;
CREATE TABLE meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    watermark_ms INTEGER NOT NULL
) STRICT;
",
        causes = one_of(&GAP_CAUSES),
        ends = one_of(&END_KINDS),
    )
}

/// The `CREATE` statements of [`SCHEMA_VERSION`].
pub fn schema_sql() -> String {
    v1_sql() + &v2_additions_sql()
}

/// `sqlite_master` rows a complete v1 database has.
const OBJECTS_V1: i64 = 5;
/// ... and a complete v2 one: v1 plus `gaps`, `sessions`, `meta`.
const OBJECTS: i64 = OBJECTS_V1 + 3;

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
    super::open::tighten_sidecars(path).map_err(refuse)?;
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
        1 => {
            crate::sqlite_file::require_known_schema(&conn, &v1_sql()).map_err(refuse)?;
            if objects != OBJECTS_V1 {
                return Err(refuse("the schema is incomplete"));
            }
        }
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
    // Last, so a file that is refused for its layout is never modified.
    if version == 1 {
        migrate_v1_to_v2(&conn)?;
    }
    Ok(conn)
}

/// Version 1 to 2: the additions and the new version in one transaction. A
/// failure rolls back, leaving the v1 file as it was. The version is read again
/// once the write lock is held, so a migrator that lost the race does nothing.
fn migrate_v1_to_v2(conn: &Connection) -> Result<(), Unavailable> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(|e| refuse(format!("migrating the schema to version 2: {e}")))?;
    let step = (|| {
        if int(conn, "user_version").map_err(|e| e.to_string())? >= SCHEMA_VERSION {
            return Ok(());
        }
        conn.execute_batch(&format!(
            "{} PRAGMA user_version = {SCHEMA_VERSION};",
            v2_additions_sql()
        ))
        .map_err(|e| e.to_string())
    })();
    match step.and_then(|()| conn.execute_batch("COMMIT;").map_err(|e| e.to_string())) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best effort: the connection is dropped on error, which rolls back too.
            let _ = conn.execute_batch("ROLLBACK;");
            Err(refuse(format!("migrating the schema to version 2: {e}")))
        }
    }
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
        // v1: 2 tables + 2 indexes + the UNIQUE index on rules_seen.rule_hash;
        // v2 adds 3 tables.
        assert_eq!(n, OBJECTS);
    }

    fn v1_file() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&v1_sql()).unwrap();
        conn.execute_batch(
            "INSERT INTO events (ts_ms, process_path, action, answered_by) \
             VALUES (1234, '/usr/bin/curl', 'allow', 'rule'); PRAGMA user_version = 1;",
        )
        .unwrap();
        conn
    }

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn a_migration_that_fails_partway_leaves_a_valid_v1_file_that_migrates_next_time() {
        let conn = v1_file();
        let before = tables(&conn);
        // The last table of the additions already exists as something else, so
        // `gaps` and `sessions` are created before the statement that fails.
        conn.execute_batch("CREATE VIEW meta AS SELECT 1 AS id;")
            .unwrap();
        assert!(migrate_v1_to_v2(&conn).is_err());
        assert_eq!(int(&conn, "user_version").unwrap(), 1);
        assert_eq!(tables(&conn), before, "no half-migrated table is left");
        assert!(conn.is_autocommit(), "no transaction is left open");
        let events: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 1, "the v1 rows are intact");
        conn.execute_batch("DROP VIEW meta;").unwrap();
        migrate_v1_to_v2(&conn).unwrap();
        assert_eq!(int(&conn, "user_version").unwrap(), SCHEMA_VERSION);
        assert_eq!(tables(&conn).len(), before.len() + 3);
    }

    #[test]
    fn migrating_a_file_another_migrator_already_moved_to_v2_does_nothing() {
        let conn = v1_file();
        migrate_v1_to_v2(&conn).unwrap();
        migrate_v1_to_v2(&conn).unwrap();
        assert_eq!(int(&conn, "user_version").unwrap(), SCHEMA_VERSION);
    }
}
