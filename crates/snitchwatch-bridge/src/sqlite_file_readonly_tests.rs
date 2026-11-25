//! The read-only opener: it must refuse what the writable one refuses, never
//! create or change a file, and leave a connection that cannot write.

use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::time::Duration;

/// Run `f` on its own thread and fail, instead of hanging CI, if it does not
/// finish: a FIFO opened for reading blocks until a writer shows up.
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the open did not return: it hung or panicked")
}

fn make_db(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1), (2);")
        .unwrap();
}

fn sidecar(db: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn mkfifo(path: &Path) {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

#[test]
fn a_missing_file_is_not_created_and_is_a_not_found_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    let error = open_owner_only_readonly(&db).expect_err("nothing there");
    match error {
        OpenError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected NotFound, got {other}"),
    }
    assert!(!db.exists(), "a reader never creates the store");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn it_reads_an_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    make_db(&db);
    let conn = open_owner_only_readonly(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT sum(x) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
}

#[test]
fn it_does_not_change_mode_or_mtime_of_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    make_db(&db);
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o640)).unwrap();
    let before = std::fs::metadata(&db).unwrap();
    let conn = open_owner_only_readonly(&db).unwrap();
    conn.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
        .unwrap();
    drop(conn);
    let after = std::fs::metadata(&db).unwrap();
    assert_eq!(after.mode() & 0o7777, 0o640, "no set_permissions");
    assert_eq!(after.mtime_nsec(), before.mtime_nsec());
    assert_eq!(after.mtime(), before.mtime());
}

#[test]
fn it_refuses_a_fifo_a_symlink_and_a_hard_link_without_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo.db");
    mkfifo(&fifo);
    let result = within(move || open_owner_only_readonly(&fifo).map(drop));
    assert!(result.is_err(), "a FIFO is not a store");

    let real = dir.path().join("real.db");
    make_db(&real);
    let link = dir.path().join("link.db");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(open_owner_only_readonly(&link).is_err(), "a symlink");

    let hard = dir.path().join("hard.db");
    std::fs::hard_link(&real, &hard).unwrap();
    let error = open_owner_only_readonly(&hard).expect_err("hard link");
    assert!(error.to_string().contains("hard link"), "{error}");
}

#[test]
fn it_refuses_an_unsafe_sidecar_and_leaves_it_alone() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history.db");
        make_db(&db);
        let side = sidecar(&db, suffix);
        mkfifo(&side);
        let target = db.clone();
        let result = within(move || open_owner_only_readonly(&target).map(drop));
        let error = result.expect_err(suffix);
        assert!(error.to_string().contains(suffix), "{error}");
        assert!(std::fs::symlink_metadata(&side)
            .unwrap()
            .file_type()
            .is_fifo_like());
    }
}

trait FifoLike {
    fn is_fifo_like(&self) -> bool;
}
impl FifoLike for std::fs::FileType {
    fn is_fifo_like(&self) -> bool {
        std::os::unix::fs::FileTypeExt::is_fifo(self)
    }
}

#[test]
fn every_write_is_refused_and_the_file_is_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    make_db(&db);
    let bytes = std::fs::read(&db).unwrap();
    let before = std::fs::metadata(&db).unwrap();
    let other = dir.path().join("other.db");
    make_db(&other);
    let conn = open_owner_only_readonly(&db).unwrap();
    for sql in [
        "INSERT INTO t VALUES (3)",
        "DELETE FROM t",
        "UPDATE t SET x = 9",
        "CREATE TABLE u (y)",
        "DROP TABLE t",
        "PRAGMA journal_mode = WAL",
        "VACUUM",
        "PRAGMA query_only = OFF; INSERT INTO t VALUES (4)",
        &format!("ATTACH '{}' AS o", other.display()),
        "ATTACH ':memory:' AS m",
    ] {
        assert!(conn.execute_batch(sql).is_err(), "must be refused: {sql}");
    }
    // Not a write on a rollback-journal database: a no-op, and the file
    // comparison below proves it changed nothing.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let sum: i64 = conn
        .query_row("SELECT sum(x) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sum, 3);
    drop(conn);
    assert_eq!(std::fs::read(&db).unwrap(), bytes);
    let after = std::fs::metadata(&db).unwrap();
    assert_eq!(
        (after.mtime(), after.mtime_nsec(), after.mode()),
        (before.mtime(), before.mtime_nsec(), before.mode())
    );
    assert!(!sidecar(&db, "-journal").exists());
    assert!(!sidecar(&db, "-wal").exists());
}

#[test]
fn the_connection_is_defensive_and_does_not_wait_on_a_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    make_db(&db);
    let conn = open_owner_only_readonly(&db).unwrap();
    let ms: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ms, 0, "the reader decides how long to retry, not SQLite");
    let trusted: i64 = conn
        .query_row("PRAGMA trusted_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(trusted, 0);
    let query_only: i64 = conn
        .query_row("PRAGMA query_only", [], |r| r.get(0))
        .unwrap();
    assert_eq!(query_only, 1, "belt and braces beside READ_ONLY");
    let cell: i64 = conn
        .query_row("PRAGMA cell_size_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cell, 1);
    assert!(conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
}

/// Whether a fresh open file description would conflict with a write lock
/// anywhere in the file, i.e. whether someone in this process holds a lock.
/// OFD locks conflict with the process's classic POSIX locks (SQLite's).
///
/// `file` stays open for the whole test: closing any descriptor of the inode
/// would itself drop the locks being looked for.
fn write_lock_is_held(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `flock` is valid; `fcntl` gets a valid descriptor
    // and a pointer to it.
    let mut probe: libc::flock = unsafe { std::mem::zeroed() };
    probe.l_type = libc::F_WRLCK as _;
    probe.l_whence = libc::SEEK_SET as _;
    // SAFETY: as above.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_GETLK, &mut probe) };
    assert_eq!(rc, 0);
    probe.l_type != libc::F_UNLCK as libc::c_short
}

#[test]
fn opening_for_reading_does_not_release_a_writers_locks() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("history.db");
    make_db(&db);
    let witness = std::fs::File::open(&db).unwrap();
    let writer = Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(write_lock_is_held(&witness), "the writer's lock is visible");
    // Closing any descriptor of the inode would drop the process's locks.
    drop(open_owner_only_readonly(&db));
    assert!(
        write_lock_is_held(&witness),
        "the read-only open released the writer's lock"
    );
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn the_readonly_opener_opens_no_descriptor_of_its_own_on_the_database() {
    let source = include_str!("sqlite_file.rs");
    let start = source.find("pub fn open_owner_only_readonly").unwrap();
    let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
    for raw in ["OpenOptions", "File::open", ".open(", "drop(file)"] {
        assert!(!body.contains(raw), "the opener names {raw}");
    }
}

#[test]
fn a_huge_unexpected_catalog_is_not_read_whole() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let schema = "CREATE TABLE t (x INTEGER);";
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(schema).unwrap();
    let extra: String = (0..5_000)
        .map(|i| format!("CREATE TABLE x{i} (a);"))
        .collect();
    conn.execute_batch(&extra).unwrap();
    // The schema is parsed on first use: warm it, then count VM steps.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&steps);
    conn.progress_handler(
        1,
        Some(move || {
            counter.fetch_add(1, Ordering::Relaxed);
            false
        }),
    )
    .unwrap();
    let refused = require_known_schema(&conn, schema).unwrap_err();
    assert!(matches!(refused, OpenError::Refused(_)), "{refused}");
    let steps = steps.load(Ordering::Relaxed);
    // Reading all 5,000 rows is tens of thousands of steps.
    assert!(steps < 2_000, "{steps} VM steps for a refusal");
}
