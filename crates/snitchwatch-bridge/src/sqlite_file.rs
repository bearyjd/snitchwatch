//! Opening a store's SQLite file the way every Snitchwatch store does, in one
//! place: owner-only, refusing what SQLite would otherwise follow or hang on.
//!
//! The blocklist and profile stores live in a state directory that other
//! accounts may be able to reach, and SQLite itself opens more than the one
//! path it is given: a `-journal`, `-wal` or `-shm` next to the database. The
//! helpers here are what both stores call, so a hardening fix lands once.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

/// Why a store file can't be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Refused on purpose: something about the file isn't what a Snitchwatch
    /// store looks like. The message is a plain sentence for the user.
    #[error("{0}")]
    Refused(String),
}

impl OpenError {
    /// As an I/O error with the same message, for stores whose error type has
    /// no variant of its own for a refusal.
    pub fn into_io(self) -> std::io::Error {
        match self {
            Self::Io(e) => e,
            Self::Refused(message) => {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
            }
            Self::Sqlite(e) => std::io::Error::other(e.to_string()),
        }
    }
}

/// `Connection::open`'s flags without `URI`, plus `NOFOLLOW`: SQLite opens
/// the path again after [`open_owner_only`]'s checks.
pub(crate) fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
}

/// What SQLite opens next to a database, by name.
const SIDECAR_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

/// Open (or create) the database at `path`, owner-only (0600). The path is
/// opened with `O_NOFOLLOW` and must be a regular file; the mode is set
/// through that handle. SQLite gives its journal files the database's mode.
///
/// SQLite also opens `<path>-journal`, `-wal` and `-shm` itself, following
/// whatever is there: a FIFO named like that hangs it, a symlink is followed,
/// and another user's file is reused and can't be tightened to 0600. So an
/// existing one that isn't a regular file of ours is refused first.
///
/// The connection does not trust the schema (`PRAGMA trusted_schema = OFF`),
/// so a crafted database's views and triggers can't call functions that
/// aren't innocuous. A store should then call [`require_plain_tables`].
pub fn open_owner_only(path: &Path) -> Result<Connection, OpenError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    refuse_unsafe_sidecars(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(OpenError::Refused(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    drop(file);
    let conn = open_connection(path)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
    Ok(conn)
}

/// What matters about an existing SQLite sidecar file.
pub(crate) struct SidecarFacts {
    pub regular: bool,
    pub uid: u32,
}

/// Why a sidecar of ours-to-be can't be used, if it can't: it must be a
/// regular file (not a FIFO, symlink or directory) owned by `euid`.
pub(crate) fn sidecar_problem(facts: &SidecarFacts, euid: u32) -> Option<&'static str> {
    if !facts.regular {
        Some("isn't a regular file")
    } else if facts.uid != euid {
        Some("belongs to another user")
    } else {
        None
    }
}

fn refuse_unsafe_sidecars(path: &Path) -> Result<(), OpenError> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    let euid = unsafe { libc::geteuid() };
    for suffix in SIDECAR_SUFFIXES {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let sidecar = std::path::PathBuf::from(name);
        // `lstat`: a symlink is itself the problem, whatever it points to.
        let meta = match std::fs::symlink_metadata(&sidecar) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let facts = SidecarFacts {
            regular: meta.file_type().is_file(),
            uid: meta.uid(),
        };
        if let Some(why) = sidecar_problem(&facts, euid) {
            return Err(OpenError::Refused(format!(
                "{} {why}; SQLite would use it, so remove it and restart",
                sidecar.display()
            )));
        }
    }
    Ok(())
}

/// Refuse a database whose schema holds anything a Snitchwatch store never
/// creates: a view, a trigger or a virtual table. That is what confirms the
/// store's tables are real tables, and not, say, a view named like one with a
/// query that never ends (which makes `CREATE TABLE IF NOT EXISTS` a no-op
/// over it). A virtual table is `type = 'table'` in `sqlite_master` too, so
/// its `CREATE VIRTUAL TABLE` text is what gives it away.
///
/// Call before the store's schema is created.
pub fn require_plain_schema(conn: &Connection) -> Result<(), OpenError> {
    let mut statement = conn.prepare("SELECT type, name, sql FROM sqlite_master")?;
    let objects = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for object in objects {
        let (kind, name, sql) = object?;
        let virtual_table = kind == "table"
            && sql.is_some_and(|sql| {
                sql.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE VIRTUAL")
            });
        let kind = if virtual_table {
            "virtual table"
        } else {
            kind.as_str()
        };
        if virtual_table || kind == "view" || kind == "trigger" {
            return Err(OpenError::Refused(format!(
                "the database has a {kind} named {name}, which a Snitchwatch store never has; \
                 it was left as it is"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocklists::store::BlocklistStore;
    use crate::profiles::store::ProfileStore;
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    /// Run `f` on its own thread and fail, instead of hanging CI, if it
    /// doesn't finish: the sidecar bug is a hang.
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(value) => value,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("the open did not return: it hung")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the open panicked on its thread")
            }
        }
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

    fn message(error: OpenError) -> String {
        error.to_string()
    }

    const SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

    #[test]
    fn a_fifo_sidecar_is_refused_without_hanging() {
        for suffix in SUFFIXES {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("store.sqlite3");
            mkfifo(&sidecar(&db, suffix));
            let result = within(move || open_owner_only(&db).map(drop));
            let error = message(result.expect_err(suffix));
            assert!(error.contains(suffix), "{error}");
            assert!(error.contains("regular file"), "{error}");
        }
    }

    #[test]
    fn a_fifo_sidecar_is_refused_by_both_stores_without_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = dir.path().join("profiles.sqlite3");
        let blocklists = dir.path().join("blocklists.sqlite3");
        mkfifo(&sidecar(&profiles, "-journal"));
        mkfifo(&sidecar(&blocklists, "-journal"));
        within(move || {
            assert!(ProfileStore::open(&profiles).is_err());
            assert!(BlocklistStore::open(&blocklists).is_err());
        });
    }

    #[test]
    fn a_symlinked_sidecar_is_refused_and_its_target_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        for suffix in SUFFIXES {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("store.sqlite3");
            let target = dir.path().join("elsewhere");
            std::fs::write(&target, b"keep").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::os::unix::fs::symlink(&target, sidecar(&db, suffix)).unwrap();
            let error = open_owner_only(&db).map(drop).expect_err(suffix);
            assert!(message(error).contains(suffix));
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "the target was changed");
        }
    }

    #[test]
    fn a_dangling_symlink_or_a_directory_as_a_sidecar_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.sqlite3");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), sidecar(&db, "-journal")).unwrap();
        std::fs::create_dir(sidecar(&db, "-wal")).unwrap();
        assert!(open_owner_only(&db).is_err());
        std::fs::remove_file(sidecar(&db, "-journal")).unwrap();
        assert!(open_owner_only(&db).is_err(), "a directory as -wal");
    }

    #[test]
    fn a_regular_sidecar_of_ours_is_fine() {
        // A crash can leave a journal behind; SQLite must be able to recover.
        for suffix in SUFFIXES {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("store.sqlite3");
            std::fs::write(sidecar(&db, suffix), b"").unwrap();
            open_owner_only(&db).unwrap_or_else(|e| panic!("{suffix}: {e}"));
        }
    }

    #[test]
    fn what_makes_a_sidecar_unacceptable() {
        let mine = SidecarFacts {
            regular: true,
            uid: 1000,
        };
        assert_eq!(sidecar_problem(&mine, 1000), None);
        let theirs = SidecarFacts {
            regular: true,
            uid: 1001,
        };
        assert!(sidecar_problem(&theirs, 1000).is_some_and(|why| why.contains("another user")));
        let odd = SidecarFacts {
            regular: false,
            uid: 1000,
        };
        assert!(sidecar_problem(&odd, 1000).is_some_and(|why| why.contains("regular file")));
    }

    /// A database somebody crafted: a view where a table belongs, with a query
    /// that never ends. `CREATE TABLE IF NOT EXISTS` is a no-op over it.
    fn craft_view(path: &Path, name: &str) {
        Connection::open(path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE VIEW {name} AS WITH RECURSIVE c(x) AS \
                 (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c;"
            ))
            .unwrap();
    }

    #[test]
    fn a_view_in_place_of_a_table_is_refused_by_the_stores() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = dir.path().join("profiles.sqlite3");
        craft_view(&profiles, "profiles");
        let error = within(move || ProfileStore::open(&profiles).map(drop))
            .expect_err("a view named profiles");
        assert!(error.to_string().contains("profiles"), "{error}");

        for name in ["subscriptions", "entries"] {
            let blocklists = dir.path().join(format!("{name}.sqlite3"));
            craft_view(&blocklists, name);
            let error =
                within(move || BlocklistStore::open(&blocklists).map(drop)).expect_err(name);
            assert!(error.to_string().contains(name), "{error}");
        }
    }

    #[test]
    fn a_trigger_or_any_view_is_refused_even_beside_real_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        drop(ProfileStore::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TRIGGER t AFTER INSERT ON profiles BEGIN SELECT 1; END;")
            .unwrap();
        drop(conn);
        assert!(ProfileStore::open(&path).is_err(), "a trigger");

        let other = dir.path().join("other.sqlite3");
        drop(ProfileStore::open(&other).unwrap());
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE VIEW extra AS SELECT 1;")
            .unwrap();
        assert!(ProfileStore::open(&other).is_err(), "an unrelated view");
    }

    #[test]
    fn a_virtual_table_is_refused_though_sqlite_calls_it_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE VIRTUAL TABLE profiles USING fts5(id, name);")
            .unwrap();
        let error = ProfileStore::open(&path)
            .map(drop)
            .expect_err("a virtual table");
        assert!(
            error.to_string().contains("virtual table named profiles"),
            "{error}"
        );
    }

    #[test]
    fn real_tables_pass_and_a_fresh_database_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite3");
        drop(ProfileStore::open(&path).unwrap());
        let conn = open_owner_only(&path).unwrap();
        require_plain_schema(&conn).unwrap();
        let fresh = open_owner_only(&dir.path().join("fresh.sqlite3")).unwrap();
        require_plain_schema(&fresh).unwrap();
    }

    #[test]
    fn the_schema_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_owner_only(&dir.path().join("store.sqlite3")).unwrap();
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |row| row.get(0))
            .unwrap();
        assert_eq!(trusted, 0);
    }
}
