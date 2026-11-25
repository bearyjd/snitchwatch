//! The reader: pages, unavailable states, generation, cancel, and that it
//! cannot write.

use std::os::unix::fs::PermissionsExt;

use super::reader::*;
use super::reader_fixture::Fx;

fn ts_of(page: &Page) -> Vec<i64> {
    page.events.iter().map(|e| e.ts_ms).collect()
}

fn unavailable(outcome: Outcome) -> Unavail {
    match outcome {
        Outcome::Unavailable(u) => u,
        other => panic!("expected unavailable, got {other:?}"),
    }
}

#[test]
fn the_head_page_is_newest_first_with_a_cursor_and_the_rest_follows_once() {
    let fx = Fx::new();
    fx.events(1..=250);
    let first = fx.page(None, 200);
    assert!(first.at_head);
    assert_eq!(ts_of(&first), (51..=250).rev().collect::<Vec<_>>());
    assert_eq!(first.end, End::More);
    let cursor = first.next_cursor.expect("an older page exists");
    assert_eq!(cursor.ts_ms, 51);
    let second = fx.page(Some(cursor), 200);
    assert!(!second.at_head);
    assert_eq!(ts_of(&second), (1..=50).rev().collect::<Vec<_>>());
    assert!(second.next_cursor.is_none());
    assert_eq!(second.end, End::OldestRecorded);
}

#[test]
fn the_limit_is_clamped_not_refused() {
    let fx = Fx::new();
    fx.events(1..=300);
    assert_eq!(fx.page(None, 10_000).events.len(), 200);
    assert_eq!(fx.page(None, 0).events.len(), 1);
    assert_eq!(fx.page(None, 7).events.len(), 7);
}

#[test]
fn events_carry_repeat_count_and_possible_duplicate_from_the_view() {
    let fx = Fx::new();
    fx.events([5]);
    fx.conn()
        .execute("UPDATE events SET repeat_count = 9", [])
        .unwrap();
    let page = fx.page(None, 10);
    assert_eq!(page.events[0].repeat_count, 9);
    assert!(!page.events[0].possible_duplicate);
}

#[test]
fn a_missing_file_is_unavailable_and_is_not_created() {
    let fx = Fx::new();
    std::fs::remove_file(&fx.db).unwrap();
    let u = unavailable(fx.read(None, 10));
    assert_eq!(u.reason, "no history");
    assert!(!u.retry);
    assert!(!fx.db.exists());
}

#[test]
fn a_file_that_is_not_ours_is_unavailable_not_empty() {
    let fx = Fx::new();
    std::fs::write(&fx.db, b"this is not a database at all, just text").unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(u.reason.contains("history"), "{}", u.reason);
}

#[test]
fn a_schema_with_something_extra_is_refused() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch("CREATE VIEW sneaky AS SELECT * FROM events;")
        .unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(
        u.reason.contains("schema") || u.reason.contains("sneaky"),
        "{}",
        u.reason
    );
}

#[test]
fn a_schema_with_something_missing_is_refused() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch("DROP VIEW history_events;")
        .unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(u.reason.contains("schema"), "{}", u.reason);
}

#[test]
fn another_schema_version_is_unavailable_not_read() {
    let fx = Fx::new();
    fx.conn().pragma_update(None, "user_version", 3).unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(u.reason.contains("version"), "{}", u.reason);
}

#[test]
fn a_history_directory_that_is_not_private_is_unavailable() {
    let fx = Fx::new();
    let dir = fx.db.parent().unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(u.reason.contains("700"), "{}", u.reason);
}

#[test]
fn a_crashed_writers_hot_journal_is_unavailable_retry_and_left_untouched() {
    let fx = Fx::new();
    fx.events(1..=3000);
    let w = fx.conn();
    w.execute_batch("PRAGMA cache_size = 2; BEGIN;").unwrap();
    w.execute("UPDATE events SET dst_host = printf('%0200d', id)", [])
        .unwrap();
    // A copy taken mid-transaction: nobody holds a lock on it, and its
    // journal is hot (a crashed writer's).
    let root = super::test_support::tempdir();
    let dir = root.path().join("history");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = dir.join("history.db");
    std::fs::copy(&fx.db, &db).unwrap();
    let journal = dir.join("history.db-journal");
    std::fs::copy(fx.db.with_extension("db-journal"), &journal).unwrap();
    w.execute_batch("ROLLBACK;").unwrap();
    assert!(
        std::fs::metadata(&journal).unwrap().len() > 0,
        "a journal to be hot"
    );
    let (db_before, journal_before) = (
        std::fs::read(&db).unwrap(),
        std::fs::read(&journal).unwrap(),
    );

    let copy = super::locate(super::Mode::User, Some(root.path())).unwrap();
    std::fs::copy(fx.rig.location.settings_path(), copy.settings_path()).unwrap();
    let ctx = Context {
        location: &copy,
        ..fx.ctx()
    };
    let outcome = read_page(
        &Request {
            cursor: None,
            limit: 10,
        },
        &ctx,
    );
    let u = unavailable(outcome);
    assert!(
        u.retry,
        "a journal to recover: retry, not empty: {}",
        u.reason
    );
    assert_eq!(
        std::fs::read(&journal).unwrap(),
        journal_before,
        "journal untouched"
    );
    assert_eq!(std::fs::read(&db).unwrap(), db_before, "database untouched");
}

#[test]
fn a_cursor_from_before_a_generation_bump_is_refused() {
    let fx = Fx::new();
    fx.events(1..=300);
    let cursor = fx.page(None, 100).next_cursor.unwrap();
    fx.generation.advance_for_test().unwrap();
    assert!(matches!(
        fx.read(Some(cursor), 100),
        Outcome::StaleGeneration
    ));
    // Starting again from the head works under the new generation.
    let page = fx.page(None, 100);
    assert_eq!(page.generation, fx.generation.current());
}

#[test]
fn a_page_that_finishes_after_the_generation_moved_is_dropped() {
    let fx = Fx::new();
    fx.events(1..=300);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 100,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    read.load_ledger().unwrap_or_else(|o| panic!("{o:?}"));
    fx.generation.advance_for_test().unwrap();
    assert!(matches!(read.finish(), Outcome::Superseded));
}

#[test]
fn a_clear_between_the_events_and_the_ledger_delivers_nothing() {
    let fx = Fx::new();
    fx.events(1..=300);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 100,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    read.load_events().unwrap_or_else(|o| panic!("{o:?}"));
    fx.generation.advance_for_test().unwrap();
    // The next stage notices at once; nothing is built from the old world.
    assert!(matches!(read.load_ledger(), Err(Outcome::Superseded)));
}

#[test]
fn a_cancel_before_the_read_starts_runs_no_statement() {
    let fx = Fx::new();
    fx.events(1..=300);
    fx.cancel.cancel();
    assert!(matches!(fx.read(None, 100), Outcome::Cancelled));
}

#[test]
fn a_cancel_between_stages_stops_the_read() {
    let fx = Fx::new();
    fx.events(1..=300);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 100,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    fx.cancel.cancel();
    assert!(matches!(read.load_events(), Err(Outcome::Cancelled)));
}

#[test]
fn a_full_read_session_leaves_the_files_byte_identical() {
    let fx = Fx::new();
    fx.events(1..=500);
    fx.gap(10, 20, super::ledger::GapCause::UpstreamLoss, 3);
    let bytes = std::fs::read(&fx.db).unwrap();
    let meta = std::fs::metadata(&fx.db).unwrap();
    for _ in 0..3 {
        let _ = fx.all_pages(100);
    }
    assert_eq!(std::fs::read(&fx.db).unwrap(), bytes);
    let after = std::fs::metadata(&fx.db).unwrap();
    assert_eq!(
        (after.permissions().mode(), after.modified().unwrap()),
        (meta.permissions().mode(), meta.modified().unwrap())
    );
    assert!(!fx.db.with_extension("db-journal").exists());
}

#[test]
fn an_object_with_the_right_name_and_other_text_is_refused() {
    let fx = Fx::new();
    fx.conn()
        .execute_batch(
            "DROP VIEW history_events; \
             CREATE VIEW history_events AS SELECT e.*, 0 AS possible_duplicate FROM events e;",
        )
        .unwrap();
    let u = unavailable(fx.read(None, 10));
    assert!(u.reason.contains("isn't the one"), "{}", u.reason);
}

fn locked_writer(fx: &Fx) -> rusqlite::Connection {
    let writer = rusqlite::Connection::open(&fx.db).unwrap();
    writer.busy_timeout(std::time::Duration::ZERO).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    writer
}

#[test]
fn a_writer_mid_commit_makes_a_read_unavailable_retry_never_an_empty_page() {
    let fx = Fx::new();
    fx.events(1..=100);
    let writer = locked_writer(&fx);
    let u = unavailable(fx.read(None, 10));
    assert!(u.retry, "{}", u.reason);
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        fx.page(None, 10).events.len(),
        10,
        "and it reads once the writer is done"
    );
}

#[test]
fn a_writer_arriving_between_stages_before_any_event_is_unavailable_retry() {
    let fx = Fx::new();
    fx.events(1..=100);
    let ctx = fx.ctx();
    let request = Request {
        cursor: None,
        limit: 50,
    };
    let mut read = Read::open(&request, &ctx).unwrap_or_else(|o| panic!("{o:?}"));
    let writer = locked_writer(&fx);
    // Nothing was read: never a successful empty page with no progress.
    let loaded = read.load_events();
    assert!(
        matches!(loaded, Err(Outcome::Unavailable(ref u)) if u.retry),
        "{loaded:?}"
    );
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fx.page(None, 50).events.len(), 50, "and it reads again");
}
