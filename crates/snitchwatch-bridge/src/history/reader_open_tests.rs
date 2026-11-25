//! A lock during open or schema validation is a writer's commit window, never
//! a refusal; a schema that really is wrong stays refused.

use std::time::Duration;

use super::reader_fixture::Fx;
use super::reader_open::{open_validated, schema_unavailable};
use super::reader_sql::Control;
use super::schema::schema_sql;
use crate::sqlite_file::{require_known_schema, schema_object_count, OpenError};

fn locked_writer(fx: &Fx) -> rusqlite::Connection {
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    writer
}

fn reader_conn(fx: &Fx) -> rusqlite::Connection {
    let conn =
        rusqlite::Connection::open_with_flags(&fx.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    conn
}

#[test]
fn a_lock_while_the_schema_is_read_is_retryable_in_every_schema_step() {
    let fx = Fx::new();
    let writer = locked_writer(&fx);
    let conn = reader_conn(&fx);
    let schema = schema_sql();
    let known = require_known_schema(&conn, &schema).unwrap_err();
    assert!(matches!(known, OpenError::Sqlite(_)), "{known:?}");
    let u = schema_unavailable(known);
    assert!(u.retry, "{}", u.reason);
    // The object count reads the in-memory reference, so it cannot be busy;
    // a busy error from it still maps the same way.
    assert!(schema_object_count(&schema).is_ok());
    let busy =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None);
    assert!(schema_unavailable(OpenError::Sqlite(busy)).retry);
    let locked = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
        None,
    );
    assert!(schema_unavailable(OpenError::Sqlite(locked)).retry);
    drop(writer);
}

#[test]
fn opening_under_an_exclusive_lock_retries_then_succeeds_once_released() {
    let fx = Fx::new();
    fx.events(1..=10);
    let ctx = fx.ctx();
    let control = Control::new(ctx.cancel, &ctx.limits);
    let writer = locked_writer(&fx);
    let err = open_validated(&fx.db, &control, Duration::ZERO).unwrap_err();
    assert!(err.retry, "{}", err.reason);
    writer.execute_batch("ROLLBACK").unwrap();
    open_validated(&fx.db, &control, Duration::ZERO).expect("opens once released");
}

#[test]
fn a_really_unknown_schema_is_still_refused_for_good() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch("CREATE TABLE intruder (x)")
        .unwrap();
    let ctx = fx.ctx();
    let control = Control::new(ctx.cancel, &ctx.limits);
    let err = open_validated(&fx.db, &control, Duration::ZERO).unwrap_err();
    assert!(!err.retry, "{}", err.reason);
    assert!(err.reason.contains("refused"), "{}", err.reason);
}

/// A catalog far bigger than the history schema: reading it takes real work,
/// done under the read lock.
fn huge_catalog(fx: &Fx, tables: u32) {
    let extra: String = (0..tables)
        .map(|i| format!("CREATE TABLE x{i} (a, b, c);"))
        .collect();
    fx.conn()
        .execute_batch("PRAGMA max_page_count = 1000000")
        .unwrap();
    fx.conn()
        .execute_batch(&format!("BEGIN; {extra} COMMIT;"))
        .unwrap();
}

#[test]
fn a_cancel_during_the_schema_check_interrupts_it_instead_of_waiting_it_out() {
    let fx = Fx::new();
    huge_catalog(&fx, 4_000);
    let ctx = fx.ctx();
    let control = Control::new(ctx.cancel, &ctx.limits);
    let err = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(2));
            ctx.cancel.cancel();
        });
        open_validated(&fx.db, &control, Duration::ZERO).unwrap_err()
    });
    // Interrupted at a tick; not carried through to a verdict on the catalog.
    assert_eq!(err.reason, "history read was interrupted", "{}", err.reason);
}

#[test]
fn a_huge_unexpected_catalog_is_refused_for_good_not_retried() {
    let fx = Fx::new();
    huge_catalog(&fx, 2_500);
    let ctx = fx.ctx();
    let control = Control::new(ctx.cancel, &ctx.limits);
    let err = open_validated(&fx.db, &control, Duration::ZERO).unwrap_err();
    assert!(!err.retry, "retrying cannot make it the history database");
    // Either verdict (too big to check in budget, or plainly not ours) is final.
    assert!(
        err.reason.contains("never has") || err.reason.contains("too large"),
        "{}",
        err.reason
    );
}
