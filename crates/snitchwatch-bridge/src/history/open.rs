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

/// Set the mode of the directory at `dir` to 0700 without needing read
/// permission on it (a umask like 0477 creates it 0300): open it `O_PATH`
/// (no permission needed, `O_NOFOLLOW`) and `chmod` through
/// `/proc/self/fd/N`, which names that exact directory, so a swap of the path
/// afterwards changes nothing.
fn chmod_dir_700(dir: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: a valid NUL-terminated path; the fd is owned right away.
    let raw = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` is a freshly opened descriptor nothing else owns.
    let handle = unsafe { OwnedFd::from_raw_fd(raw) };
    let magic = CString::new(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(&handle)
    ))
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: a valid NUL-terminated path.
    if unsafe { libc::chmod(magic.as_ptr(), DIR_MODE) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The history directory, created 0700 if it isn't there, and checked either
/// way. The parent (the state directory) must exist; it is never created
/// here.
pub(crate) fn ensure_dir(dir: &Path, euid: u32) -> Result<(), Unavailable> {
    match fs::DirBuilder::new().mode(DIR_MODE).create(dir) {
        // `mkdir` applies the umask, which can only narrow the mode (to 0300
        // under 0477, which can't be reopened to fix): make it exactly 0700.
        Ok(()) => chmod_dir_700(dir).map_err(|e| io_reason("the history directory", dir, &e))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_reason("couldn't create the history directory", dir, &e)),
    }
    check_dir(dir, euid)
}

fn io_failure(sidecar: &Path, e: &std::io::Error) -> std::io::Error {
    std::io::Error::new(
        e.kind(),
        format!(
            "couldn't check the permissions of {}: {e}",
            sidecar.display()
        ),
    )
}

/// Make each existing SQLite sidecar of `db` exactly 0600, through a handle
/// opened `O_NOFOLLOW | O_NONBLOCK`. One that isn't a regular file of ours is
/// left for [`crate::sqlite_file::open_owner_only`] to refuse.
pub(crate) fn tighten_sidecars(db: &Path) -> Result<(), crate::sqlite_file::OpenError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        let sidecar = std::path::PathBuf::from(name);
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&sidecar)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // A symlink (ELOOP): the opener refuses it.
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => continue,
            // Anything else (a 0244 file we can't read, say) means the mode
            // can't be checked or fixed: fail closed rather than let a
            // group/other-readable sidecar survive the open.
            Err(e) => return Err(io_failure(&sidecar, &e).into()),
        };
        let meta = file.metadata()?;
        // Type, owner and link count are checked **before** any chmod: a hard
        // link to someone else's file must not have its mode changed (the
        // opener refuses it afterwards).
        if meta.is_file()
            && meta.uid() == effective_uid()
            && meta.nlink() == 1
            && meta.mode() & 0o7777 != 0o600
        {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// The open history database. Only [`super::open_store`] makes one, and only
/// while history is enabled.
pub struct Store {
    conn: rusqlite::Connection,
}

impl Store {
    /// The connection: `secure_delete` on, `journal_mode` DELETE, layout
    /// verified. The writer (PR 2) is its only user.
    pub(crate) fn into_connection(self) -> rusqlite::Connection {
        self.conn
    }

    pub fn connection(&self) -> &rusqlite::Connection {
        &self.conn
    }

    pub(crate) fn open(location: &Location, euid: u32) -> Result<Self, Unavailable> {
        check_dir(location.dir(), euid)?;
        let conn = schema::prepare(&location.db_path())?;
        Ok(Self { conn })
    }
}
