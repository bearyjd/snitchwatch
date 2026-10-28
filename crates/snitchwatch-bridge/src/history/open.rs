//! The history directory and database, opened the strict way: the directory
//! must be exactly 0700 and the bridge's own (the per-user state check
//! tolerates 0755 and is not enough here), and the database goes through
//! [`crate::sqlite_file::open_owner_only`].
//!
//! Only the history directory is checked, not its parents, like the state
//! directory itself (see `bridge-cli`'s `storage.rs`): the bridge opens the
//! database by path after checking the directory by handle, so whoever can
//! write above it could swap it. The system bridge's parents are root's.

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use super::{schema, Location, Unavailable};

/// What matters about the history directory.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirFacts {
    pub is_dir: bool,
    pub uid: u32,
    /// `st_mode`, file type bits included.
    pub mode: u32,
}

/// The exact mode every history directory must have.
const DIR_MODE: u32 = 0o700;

/// Pure rule: a directory, ours, with permission bits (setuid, setgid and
/// sticky included) of exactly 0700.
pub(crate) fn check_facts(facts: &DirFacts, euid: u32) -> Result<(), Unavailable> {
    if !facts.is_dir {
        return Err(Unavailable::new("the history path is not a directory"));
    }
    if facts.uid != euid {
        return Err(Unavailable::new(
            "the history directory is not owned by the user running the bridge",
        ));
    }
    let bits = facts.mode & 0o7777;
    if bits != DIR_MODE {
        return Err(Unavailable::new(format!(
            "the history directory has mode {bits:o}, not exactly 700"
        )));
    }
    Ok(())
}

pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}

fn io_reason(what: &str, dir: &Path, e: &std::io::Error) -> Unavailable {
    Unavailable::new(format!("{what} {}: {e}", dir.display()))
}

/// Whether anything is at `dir`, judged without following a link. A link or a
/// file there counts as present (so it is then refused), never as absent.
pub(crate) fn dir_exists(dir: &Path) -> Result<bool, Unavailable> {
    match fs::symlink_metadata(dir) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_reason("the history directory", dir, &e)),
    }
}

/// Open `dir` `O_NOFOLLOW | O_DIRECTORY` and check what that one handle says.
fn open_checked(dir: &Path, euid: u32) -> Result<fs::File, Unavailable> {
    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
        .map_err(|e| io_reason("the history directory", dir, &e))?;
    let meta = handle
        .metadata()
        .map_err(|e| io_reason("the history directory", dir, &e))?;
    check_facts(
        &DirFacts {
            is_dir: meta.is_dir(),
            uid: meta.uid(),
            mode: meta.mode(),
        },
        euid,
    )?;
    Ok(handle)
}

pub(crate) fn check_dir(dir: &Path, euid: u32) -> Result<(), Unavailable> {
    open_checked(dir, euid).map(drop)
}

/// The history directory, created 0700 if it isn't there, and checked either
/// way. The parent (the state directory) must exist; it is never created
/// here.
pub(crate) fn ensure_dir(dir: &Path, euid: u32) -> Result<(), Unavailable> {
    match fs::DirBuilder::new().mode(DIR_MODE).create(dir) {
        Ok(()) => {
            // `mkdir` applies the umask, which can only narrow the mode;
            // make it exactly 0700, through a handle to what was created.
            let handle = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(dir)
                .map_err(|e| io_reason("the history directory", dir, &e))?;
            handle
                .set_permissions(fs::Permissions::from_mode(DIR_MODE))
                .map_err(|e| io_reason("the history directory", dir, &e))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_reason("couldn't create the history directory", dir, &e)),
    }
    check_dir(dir, euid)
}

/// The open history database. Only [`super::open_store`] makes one, and only
/// while history is enabled.
pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// The connection: `secure_delete` on, `journal_mode` DELETE, layout
    /// verified. The writer (PR 2) is its only user.
    pub fn connection(&self) -> &rusqlite::Connection {
        &self.conn
    }

    pub(crate) fn open(location: &Location, euid: u32) -> Result<Self, Unavailable> {
        check_dir(location.dir(), euid)?;
        let conn = schema::prepare(&location.db_path())?;
        Ok(Self { conn })
    }
}
