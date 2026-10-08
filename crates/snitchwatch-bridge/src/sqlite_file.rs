//! Opening a store's SQLite file the way every Snitchwatch store does, in one
//! place: owner-only, refusing what SQLite would otherwise follow or hang on.
//!
//! The blocklist and profile stores live in a state directory that other
//! accounts may be able to reach, and SQLite itself opens more than the one
//! path it is given: a `-journal`, `-wal` or `-shm` next to the database. The
//! helpers here are what both stores call, so a hardening fix lands once.

use std::path::Path;

use rusqlite::config::DbConfig;
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
/// opened with `O_NOFOLLOW` and must be a regular file of ours with no other
/// hard link; only then is the mode set, through that handle. SQLite gives
/// its journal files the database's mode.
///
/// SQLite also opens `<path>-journal`, `-wal` and `-shm` itself, following
/// whatever is there: a FIFO named like that hangs it, a symlink is followed,
/// and another user's file is reused and can't be tightened to 0600. So an
/// existing one that isn't a regular file of ours is refused first.
///
/// The connection does not trust the schema (`PRAGMA trusted_schema = OFF`),
/// so a crafted database's views and triggers can't call functions that
/// aren't innocuous; it is defensive (`SQLITE_DBCONFIG_DEFENSIVE`: no
/// `writable_schema`, no direct writes to shadow tables) and checks cell
/// sizes as it reads pages (`cell_size_check`), which catches a corrupt or
/// crafted page earlier. A store should then call [`require_known_schema`].
pub fn open_owner_only(path: &Path) -> Result<Connection, OpenError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    refuse_unsafe_sidecars(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    let facts = FileFacts {
        regular: meta.is_file(),
        uid: meta.uid(),
        links: meta.nlink(),
    };
    if let Some(why) = file_problem(&facts, effective_uid()) {
        return Err(OpenError::Refused(format!(
            "{} {why}, so it was left as it is",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    drop(file);
    let conn = open_connection(path)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA cell_size_check = ON;")?;
    Ok(conn)
}

/// What matters about an existing store file or SQLite sidecar file.
pub(crate) struct FileFacts {
    pub regular: bool,
    pub uid: u32,
    /// Hard links (`st_nlink`).
    pub links: u64,
}

/// Why a store file, or a sidecar of one, can't be used, if it can't: it
/// must be a regular file (not a FIFO, symlink or directory) owned by `euid`
/// whose only name is this one. Through another hard link SQLite would write
/// into some other file of ours.
pub(crate) fn file_problem(facts: &FileFacts, euid: u32) -> Option<&'static str> {
    if !facts.regular {
        Some("isn't a regular file")
    } else if facts.uid != euid {
        Some("belongs to another user")
    } else if facts.links != 1 {
        Some("has a hard link elsewhere")
    } else {
        None
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}

fn refuse_unsafe_sidecars(path: &Path) -> Result<(), OpenError> {
    use std::os::unix::fs::MetadataExt;
    let euid = effective_uid();
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
        let facts = FileFacts {
            regular: meta.file_type().is_file(),
            uid: meta.uid(),
            links: meta.nlink(),
        };
        if let Some(why) = file_problem(&facts, euid) {
            return Err(OpenError::Refused(format!(
                "{} {why}; SQLite would use it, so remove it and restart",
                sidecar.display()
            )));
        }
    }
    Ok(())
}

/// One `sqlite_master` row: type, name, table and `CREATE` text (`None` for
/// an index SQLite makes itself). Its `rootpage` differs from file to file.
type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(conn: &Connection) -> rusqlite::Result<Vec<SchemaRow>> {
    let mut statement = conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_master")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    rows.collect()
}

/// Refuse a database whose schema holds anything the store never creates:
/// every `sqlite_master` row must be one that running the store's `schema`
/// on an empty database produces, `CREATE` text and all. So the store's
/// tables are the store's tables, not, say, a view named like one with a
/// query that never ends (which makes `CREATE TABLE IF NOT EXISTS` a no-op
/// over it), a virtual table however its `CREATE` is spelled, or a table
/// with a CHECK, default or generated column the store didn't write; and
/// there is no trigger, extra index or extra table.
///
/// Fewer rows are fine: a new file has none, and a crash between two
/// `CREATE TABLE`s leaves some. `schema` is the only one a store has ever
/// written (version 1); a store that changes it must accept its old text
/// too. `sqlite_master` keeps the `CREATE` text byte for byte, so even
/// reindenting a store's `SCHEMA` would refuse every existing file.
///
/// Call before the store's schema is created.
pub fn require_known_schema(conn: &Connection, schema: &str) -> Result<(), OpenError> {
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(schema)?;
    let known = schema_rows(&reference)?;
    for row in schema_rows(conn)? {
        if known.contains(&row) {
            continue;
        }
        let (kind, name, ..) = row;
        let message = if known.iter().any(|k| k.0 == kind && k.1 == name) {
            format!(
                "the database's {kind} named {name} isn't the one a Snitchwatch store \
                 creates; it was left as it is"
            )
        } else {
            format!(
                "the database has a {kind} named {name}, which a Snitchwatch store never \
                 has; it was left as it is"
            )
        };
        return Err(OpenError::Refused(message));
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

    /// PR #104 review: a store busy at start (another process, a backup)
    /// waits instead of failing the open at once.
    #[test]
    fn stores_wait_for_a_busy_database() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_owner_only(&dir.path().join("x.sqlite3")).unwrap();
        let ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ms, 5000);
    }

    /// SQLite deletes a `-journal` or `-wal` beside an *empty* database
    /// without reading it, so a FIFO only hangs it next to one that has a
    /// table: the fixtures make one first.
    #[test]
    fn a_fifo_sidecar_is_refused_without_hanging() {
        for suffix in SUFFIXES {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("store.sqlite3");
            Connection::open(&db)
                .unwrap()
                .execute_batch("CREATE TABLE t (x);")
                .unwrap();
            mkfifo(&sidecar(&db, suffix));
            let result = within(move || {
                let conn = open_owner_only(&db)?;
                conn.query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))?;
                Ok::<_, OpenError>(())
            });
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
        drop(ProfileStore::open(&profiles).unwrap());
        drop(BlocklistStore::open(&blocklists).unwrap());
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
    fn what_makes_a_store_file_unacceptable() {
        let mine = FileFacts {
            regular: true,
            uid: 1000,
            links: 1,
        };
        assert_eq!(file_problem(&mine, 1000), None);
        let theirs = FileFacts { uid: 1001, ..mine };
        assert!(file_problem(&theirs, 1000).is_some_and(|why| why.contains("another user")));
        let odd = FileFacts {
            regular: false,
            ..mine
        };
        assert!(file_problem(&odd, 1000).is_some_and(|why| why.contains("regular file")));
        for links in [0, 2, 5] {
            let linked = FileFacts { links, ..mine };
            assert!(
                file_problem(&linked, 1000).is_some_and(|why| why.contains("hard link")),
                "{links} links"
            );
        }
    }

    /// The database itself is checked as its sidecars are, before its mode is
    /// touched: a hard link elsewhere would otherwise be made 0600 and then
    /// written to as a database.
    #[test]
    fn a_hard_linked_database_is_refused_and_its_other_name_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, b"keep").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o644)).unwrap();
        let db = dir.path().join("store.sqlite3");
        std::fs::hard_link(&other, &db).unwrap();
        let error = message(open_owner_only(&db).map(drop).expect_err("a hard link"));
        assert!(error.contains("hard link"), "{error}");
        assert!(error.contains("store.sqlite3"), "{error}");
        assert_eq!(std::fs::read(&other).unwrap(), b"keep");
        let mode = std::fs::metadata(&other).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the other name's mode was changed");
    }

    #[test]
    fn the_connection_is_defensive_and_checks_cell_sizes() {
        use rusqlite::config::DbConfig;
        let dir = tempfile::tempdir().unwrap();
        let conn = open_owner_only(&dir.path().join("store.sqlite3")).unwrap();
        assert!(conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
        let check: i64 = conn
            .query_row("PRAGMA cell_size_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, 1);
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
            error.to_string().contains("table named profiles"),
            "{error}"
        );
    }

    /// SQLite writes `CREATE VIRTUAL TABLE` itself, but it reads the schema
    /// with its tokenizer: a comment or a tab between the words, planted
    /// straight into `sqlite_master`, still loads as a virtual table. fts5's
    /// own tables are dropped from the schema, so the one row named
    /// `profiles`, of type `table`, is all that gives it away.
    #[test]
    fn a_virtual_table_spelled_with_a_comment_or_a_tab_is_refused() {
        for spelling in [
            "CREATE/**/VIRTUAL TABLE profiles USING fts5(id, name)",
            "CREATE\tVIRTUAL TABLE profiles USING fts5(id, name)",
            "create virtual/* */table profiles USING fts5(id, name)",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("profiles.sqlite3");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE VIRTUAL TABLE profiles USING fts5(id, name); \
                 PRAGMA writable_schema = ON; \
                 DELETE FROM sqlite_master WHERE name <> 'profiles';",
            )
            .unwrap();
            conn.execute(
                "UPDATE sqlite_master SET sql = ?1 WHERE name = 'profiles'",
                [spelling],
            )
            .unwrap();
            drop(conn);
            let conn = Connection::open(&path).unwrap();
            let rows = schema_rows(&conn).unwrap();
            let planted = (
                "table".to_owned(),
                "profiles".to_owned(),
                "profiles".to_owned(),
                Some(spelling.to_owned()),
            );
            assert_eq!(rows, vec![planted], "the planted schema didn't stick");
            let kind: String = conn
                .query_row(
                    "SELECT type FROM pragma_table_list WHERE name = 'profiles'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(kind, "virtual", "{spelling} didn't load as a virtual table");
            drop(conn);
            let error = ProfileStore::open(&path).map(drop).expect_err(spelling);
            assert!(
                error.to_string().contains("profiles"),
                "{spelling}: {error}"
            );
        }
    }

    /// The schema a store must match, as version 1 of each wrote it, byte for
    /// byte: `sqlite_master` keeps the `CREATE` text as written, so changing
    /// even the whitespace of a store's `SCHEMA` would refuse every existing
    /// database. These copies are frozen on purpose.
    const PROFILES_V1: &str = r#"
CREATE TABLE IF NOT EXISTS profiles (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    network_matchers  TEXT NOT NULL,
    rules             TEXT NOT NULL,
    active            INTEGER NOT NULL DEFAULT 0
);
"#;

    const SUBSCRIPTIONS_V1: &str = r#"
CREATE TABLE IF NOT EXISTS subscriptions (
    id                   TEXT PRIMARY KEY,
    url                  TEXT NOT NULL,
    display_name         TEXT NOT NULL,
    format_hint          TEXT,
    refresh_interval_secs INTEGER NOT NULL,
    last_fetched_at      TEXT,
    last_attempt_at      TEXT,
    last_fetch_status    TEXT NOT NULL,
    last_fetch_reason    TEXT,
    entry_count          INTEGER NOT NULL DEFAULT 0
);
"#;

    const ENTRIES_V1: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    subscription_id TEXT NOT NULL,
    host            TEXT NOT NULL,
    PRIMARY KEY (subscription_id, host),
    FOREIGN KEY (subscription_id) REFERENCES subscriptions(id) ON DELETE CASCADE
) WITHOUT ROWID;
"#;

    fn database(path: &Path, sql: &str) {
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    /// Anything a store doesn't create itself is refused, though each is a
    /// plain table or index: a CHECK, a different default, a generated
    /// column, an extra or expression index, an extra table.
    #[test]
    fn anything_the_profile_store_does_not_create_is_refused() {
        let altered = |from: &str, to: &str| {
            let sql = PROFILES_V1.replace(from, to);
            assert_ne!(sql, PROFILES_V1, "{from} isn't in the schema");
            sql
        };
        let cases = [
            (
                "a CHECK",
                altered(
                    "name              TEXT NOT NULL,",
                    "name              TEXT NOT NULL CHECK (length(name) < 99),",
                ),
            ),
            (
                "another default",
                altered("NOT NULL DEFAULT 0", "NOT NULL DEFAULT 1"),
            ),
            (
                "a generated column",
                altered(
                    "active            INTEGER NOT NULL DEFAULT 0",
                    "active            INTEGER NOT NULL DEFAULT 0,\n    g AS (length(name))",
                ),
            ),
            (
                "an extra index",
                format!("{PROFILES_V1}CREATE INDEX extra ON profiles(name);"),
            ),
            (
                "an expression index",
                format!("{PROFILES_V1}CREATE INDEX extra ON profiles(length(rules));"),
            ),
            (
                "an extra table",
                format!("{PROFILES_V1}CREATE TABLE extra (x);"),
            ),
        ];
        for (what, sql) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("profiles.sqlite3");
            database(&path, &sql);
            let before = std::fs::read(&path).unwrap();
            let error = ProfileStore::open(&path).map(drop).expect_err(what);
            assert!(
                error.to_string().contains("Snitchwatch store"),
                "{what}: {error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{what}: changed");
        }
    }

    #[test]
    fn anything_the_blocklist_store_does_not_create_is_refused() {
        let real = format!("{SUBSCRIPTIONS_V1}{ENTRIES_V1}");
        for (what, sql) in [
            (
                "an index on entries",
                format!("{real}CREATE INDEX idx_entries_sub ON entries(subscription_id);"),
            ),
            (
                "another default",
                real.replace("DEFAULT 0", "DEFAULT (random())"),
            ),
            ("an extra table", format!("{real}CREATE TABLE extra (x);")),
            (
                "a rowid entries table",
                real.replace(") WITHOUT ROWID;", ");"),
            ),
        ] {
            assert_ne!(sql, real, "{what}");
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("blocklists.sqlite3");
            database(&path, &sql);
            assert!(BlocklistStore::open(&path).is_err(), "{what}");
        }
    }

    /// What the stores have written since they were first saved to a file
    /// opens, with its rows: the version-1 schema (with `user_version` 1, or
    /// 0 when a crash came before it was set), only part of it (a crash between
    /// two `CREATE TABLE`s), and nothing at all (a new file).
    #[test]
    fn every_schema_a_store_has_written_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        for (name, version) in [("v1.sqlite3", 1), ("v0.sqlite3", 0)] {
            let path = dir.path().join(format!("profiles-{name}"));
            database(
                &path,
                &format!(
                    "{PROFILES_V1}INSERT INTO profiles VALUES ('home', 'Home', '[]', '[]', 1);\
                     PRAGMA user_version = {version};"
                ),
            );
            let store = ProfileStore::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(store.list_profiles().unwrap().len(), 1, "{name}");

            let path = dir.path().join(format!("blocklists-{name}"));
            database(
                &path,
                &format!("{SUBSCRIPTIONS_V1}{ENTRIES_V1}PRAGMA user_version = {version};"),
            );
            BlocklistStore::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let partial = dir.path().join("blocklists-partial.sqlite3");
        database(&partial, SUBSCRIPTIONS_V1);
        drop(BlocklistStore::open(&partial).unwrap());
        // A new file, then what the store itself wrote into it.
        for _ in 0..2 {
            drop(ProfileStore::open(&dir.path().join("new-profiles.sqlite3")).unwrap());
            drop(BlocklistStore::open(&dir.path().join("new-blocklists.sqlite3")).unwrap());
        }
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
