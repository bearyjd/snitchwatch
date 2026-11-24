//! Schema version 4 (PR 8, finding F1): `meta.expiry_debt_milli`, the persisted
//! expiry budget. A v4 file is made by a new file or by migrating v1, v2 or v3
//! in one transaction; a kill at any step leaves the older file usable.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use rusqlite::Connection;

use super::budget::Budget;
use super::schema::{migrate, prepare, schema_sql, AFTER_STEP, SCHEMA_VERSION};
use super::test_support::tempdir;

const CHILD_PATH: &str = "SW_SCHEMA_V4_PATH";
const CHILD_STEP: &str = "SW_SCHEMA_V4_STEP";

fn budget() -> Budget {
    Budget::new(16 * 1024 * 1024).unwrap()
}

/// A file in the pinned layout at an older `version`, with one event, one gap
/// (v2 and up) and a watermark of 777 (v2 and up).
fn old_file(path: &Path, version: i64) {
    let conn = crate::sqlite_file::open_owner_only(path).unwrap();
    conn.execute_batch("PRAGMA page_size = 4096; PRAGMA auto_vacuum = 2;")
        .unwrap();
    let schema = match version {
        1 => super::schema::v1_sql(),
        2 => super::schema::v2_sql(),
        _ => super::schema::v3_sql(),
    };
    conn.execute_batch(&format!("BEGIN; {schema}")).unwrap();
    conn.execute_batch(
        "INSERT INTO events (ts_ms, process_path, action, answered_by) \
         VALUES (1234, '/usr/bin/curl', 'allow', 'rule');",
    )
    .unwrap();
    if version >= 2 {
        conn.execute_batch(
            "INSERT INTO meta (id, watermark_ms) VALUES (1, 777); \
             INSERT INTO gaps (start_ms, end_ms, start_exact, cause, count) \
             VALUES (1, 2, 1, 'bridge-down', 1);",
        )
        .unwrap();
    }
    conn.execute_batch(&format!("PRAGMA user_version = {version}; COMMIT;"))
        .unwrap();
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn meta_columns(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_table_info('meta') ORDER BY cid")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn assert_kept_v3_data(conn: &Connection) {
    assert_eq!(count(conn, "events"), 1, "the event is kept");
    assert_eq!(count(conn, "gaps"), 1, "the gap is kept");
    let watermark: i64 = conn
        .query_row("SELECT watermark_ms FROM meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(watermark, 777);
}

#[test]
fn a_new_file_is_created_at_version_four_with_the_debt_column() {
    let root = tempdir();
    let path = root.path().join("new.db");
    let conn = prepare(&path, &budget()).unwrap();
    assert_eq!(SCHEMA_VERSION, 4);
    assert_eq!(user_version(&conn), 4);
    assert_eq!(
        meta_columns(&conn),
        ["id", "watermark_ms", "expiry_debt_milli"]
    );
    conn.execute_batch("INSERT INTO meta (id, watermark_ms) VALUES (1, 5);")
        .unwrap();
    let debt: i64 = conn
        .query_row("SELECT expiry_debt_milli FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(debt, 0, "a new row starts with a full bucket");
    assert!(
        conn.execute("UPDATE meta SET expiry_debt_milli = -1", [])
            .is_err(),
        "a negative debt is refused by the schema"
    );
}

#[test]
fn a_v3_file_with_data_migrates_to_v4_and_keeps_its_rows() {
    let root = tempdir();
    let path = root.path().join("v3.db");
    old_file(&path, 3);
    let conn = prepare(&path, &budget()).unwrap();
    assert_eq!(user_version(&conn), 4);
    assert_kept_v3_data(&conn);
    let debt: i64 = conn
        .query_row("SELECT expiry_debt_milli FROM meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(debt, 0);
    crate::sqlite_file::require_known_schema(&conn, &schema_sql()).unwrap();
}

#[test]
fn older_files_all_reach_v4() {
    for version in [1, 2] {
        let root = tempdir();
        let path = root.path().join("old.db");
        old_file(&path, version);
        let conn = prepare(&path, &budget()).unwrap();
        assert_eq!(user_version(&conn), 4, "from v{version}");
        assert_eq!(count(&conn, "events"), 1);
    }
}

#[test]
fn a_second_migration_and_a_second_open_change_nothing() {
    let root = tempdir();
    let path = root.path().join("v3.db");
    old_file(&path, 3);
    let conn = prepare(&path, &budget()).unwrap();
    conn.execute("UPDATE meta SET expiry_debt_milli = 4242", [])
        .unwrap();
    migrate(&conn, 2048).unwrap();
    assert_eq!(user_version(&conn), 4);
    drop(conn);
    let again = prepare(&path, &budget()).unwrap();
    assert_eq!(user_version(&again), 4);
    let debt: i64 = again
        .query_row("SELECT expiry_debt_milli FROM meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(debt, 4242, "a reopen keeps the debt");
}

#[test]
fn a_v3_migration_that_hits_the_page_cap_restores_v3() {
    let root = tempdir();
    let path = root.path().join("v3.db");
    old_file(&path, 3);
    let conn = crate::sqlite_file::open_owner_only(&path).unwrap();
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    assert!(migrate(&conn, pages as u64 - 1).is_err());
    assert_eq!(user_version(&conn), 3);
    assert_eq!(meta_columns(&conn), ["id", "watermark_ms"]);
    assert!(conn.is_autocommit());
    assert_kept_v3_data(&conn);
}

#[test]
fn a_file_newer_than_v4_is_refused_like_v3_refused_v4() {
    let root = tempdir();
    let path = root.path().join("new.db");
    drop(prepare(&path, &budget()).unwrap());
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 5;")
        .unwrap();
    let Err(reason) = prepare(&path, &budget()) else {
        panic!("a newer schema was opened");
    };
    assert!(reason.0.contains("newer schema"), "{reason}");
}

/// The child: migrates the file at `CHILD_PATH` and kills itself after step
/// `CHILD_STEP`. Without the variables (the normal run) it does nothing.
#[test]
fn migration_child() {
    let (Ok(path), Ok(step)) = (std::env::var(CHILD_PATH), std::env::var(CHILD_STEP)) else {
        return;
    };
    let step: usize = step.parse().unwrap();
    let conn = crate::sqlite_file::open_owner_only(Path::new(&path)).unwrap();
    AFTER_STEP.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |i| {
            if i == step {
                // SAFETY: kill(2) on our own pid; no memory is touched.
                unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
            }
        }));
    });
    let _ = migrate(&conn, 2048);
}

fn kill_after_step(path: &Path, step: usize) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "history::schema_v4_tests::migration_child"])
        .env(CHILD_PATH, path)
        .env(CHILD_STEP, step.to_string())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(9), "killed after step {step}");
}

#[test]
fn a_kill_after_any_migration_step_leaves_a_file_that_opens_and_migrates() {
    for from in [1, 2, 3] {
        // Steps: the v2 additions (v1 only), v3 additions (v1, v2), v4
        // additions, the version; the last index is "all run, not committed".
        let steps = match from {
            1 => 4,
            2 => 3,
            _ => 2,
        };
        for step in 0..steps {
            let root = tempdir();
            let path = root.path().join("k.db");
            old_file(&path, from);
            kill_after_step(&path, step);
            let conn = Connection::open(&path).unwrap();
            assert_eq!(user_version(&conn), from, "v{from}, killed at {step}");
            assert_eq!(count(&conn, "events"), 1);
            drop(conn);
            let conn = prepare(&path, &budget()).unwrap();
            assert_eq!(user_version(&conn), 4, "v{from}, killed at {step}");
            assert_eq!(count(&conn, "events"), 1);
        }
    }
}
