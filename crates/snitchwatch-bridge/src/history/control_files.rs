//! PR 10: the only code that removes history files.
//!
//! Everything here works on one directory handle, opened `O_NOFOLLOW |
//! O_DIRECTORY` and checked (0700, ours) by [`open::open_checked`], and names
//! files **relative to it**: `fstatat`, `renameat` and `unlinkat` on exact,
//! compiled-in names. There is no glob, no directory listing, no recursion
//! and no path built from anything stored or requested. An entry that is not
//! a plain file (a symlink, a FIFO, a directory) is refused before anything
//! is touched, never followed.
//!
//! A store is removed by renaming `history.db` to a tombstone, then
//! unlinking the tombstone, each followed by a directory fsync. A crash
//! between the two leaves no `history.db` and a tombstone that
//! [`StoreDir::finish_leftover`] removes on the next start, enabled or not.
//!
//! A clear that records, or has rows to keep, instead builds the new store
//! beside the old one and renames it over `history.db` (see `replace`). A
//! crash before that rename leaves the old store and a staged file that
//! [`StoreDir::finish_leftover`] removes too.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use super::{open, Unavailable, DB_FILE, SETTINGS_FILE};

/// A store being removed lives under this name until it is unlinked.
pub(super) const TOMBSTONE: &str = "history.db.deleting";
/// The sidecars SQLite may leave beside `history.db`.
const SIDECARS: [&str; 3] = ["history.db-journal", "history.db-wal", "history.db-shm"];
/// The store a clear is building, until it takes the live name.
pub(super) const STAGED: &str = "history.db.new";
const STAGED_SIDECARS: [&str; 3] = [
    "history.db.new-journal",
    "history.db.new-wal",
    "history.db.new-shm",
];
/// Every name a staged store can leave.
const STAGED_NAMES: [&str; 4] = [
    STAGED,
    STAGED_SIDECARS[0],
    STAGED_SIDECARS[1],
    STAGED_SIDECARS[2],
];

/// Where a removal has got to; reported after each step completes, so a test
/// can kill the process at exactly that point. Production passes a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Checked, nothing changed yet.
    Begin,
    /// `history.db` is now the tombstone.
    Renamed,
    /// The tombstone is gone.
    TombstoneRemoved,
    /// A clear created the staged store, with its schema and no rows.
    Created,
    /// A clear committed what was accepted after the preview into it.
    Carried,
    /// The staged store is closed and synced; the old one is untouched.
    Staged,
    /// The staged store has the live name; the directory is not yet synced.
    Replaced,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FilesError {
    #[error("{0}")]
    Directory(Unavailable),
    /// An entry that is not a plain file: left exactly as it was.
    #[error("{0} is not a plain file; nothing was removed")]
    NotPlain(&'static str),
    /// A sidecar of a store that is supposed to be closed (a commit did not
    /// finish, so the next start's own recovery must see it), or a tombstone
    /// where a store is about to be replaced.
    #[error("{0} is present; nothing was removed")]
    Sidecar(&'static str),
    #[error("{0} is missing; nothing was replaced")]
    Missing(&'static str),
    /// Not the file that was created for staging, or it gained a link.
    #[error("{0} is not the file that was staged; nothing was replaced")]
    NotStaged(&'static str),
    /// The tombstone was unlinked but another name for the same file keeps
    /// the deleted history on disk.
    #[error("another link to the deleted history remains ({0})")]
    Linked(&'static str),
    #[error("{0}: {1}")]
    Io(&'static str, io::Error),
}

#[path = "control_replace.rs"]
mod replace;
#[cfg(test)]
pub(super) use replace::{with_fault, Fault};
pub(super) use replace::{FileId, Replaced};

pub(super) struct StoreDir {
    handle: fs::File,
}

fn cname(name: &'static str) -> CString {
    CString::new(name).expect("a compiled-in name has no NUL")
}

impl StoreDir {
    pub(super) fn open(dir: &Path, euid: u32) -> Result<Self, FilesError> {
        open::open_checked(dir, euid)
            .map(|handle| Self { handle })
            .map_err(FilesError::Directory)
    }

    fn fd(&self) -> i32 {
        self.handle.as_raw_fd()
    }

    /// What `name` is, judged without following a link; `None` when there is
    /// no such name.
    fn stat(&self, name: &'static str) -> Result<Option<libc::stat>, FilesError> {
        let c = cname(name);
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: valid fd, NUL-terminated name, and `st` is a stat buffer.
        let rc = unsafe {
            libc::fstatat(
                self.fd(),
                c.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(FilesError::Io(name, e))
            };
        }
        // SAFETY: `fstatat` succeeded and filled the buffer.
        Ok(Some(unsafe { st.assume_init() }))
    }

    /// `Some(is_plain_file)` when `name` exists, `None` when it does not.
    fn plain(&self, name: &'static str) -> Result<Option<bool>, FilesError> {
        Ok(self
            .stat(name)?
            .map(|st| st.st_mode & libc::S_IFMT == libc::S_IFREG))
    }

    /// How many names the file `name` has (0 when it is absent).
    fn links(&self, name: &'static str) -> Result<u64, FilesError> {
        Ok(self.stat(name)?.map_or(0, |st| st.st_nlink))
    }

    fn require_plain(&self, name: &'static str) -> Result<bool, FilesError> {
        match self.plain(name)? {
            None => Ok(false),
            Some(true) => Ok(true),
            Some(false) => Err(FilesError::NotPlain(name)),
        }
    }

    fn unlink(&self, name: &'static str) -> Result<(), FilesError> {
        let c = cname(name);
        // SAFETY: valid fd and NUL-terminated name; no directory flag.
        if unsafe { libc::unlinkat(self.fd(), c.as_ptr(), 0) } != 0 {
            return Err(FilesError::Io(name, io::Error::last_os_error()));
        }
        Ok(())
    }

    fn sync(&self) -> Result<(), FilesError> {
        self.handle
            .sync_all()
            .map_err(|e| FilesError::Io("the history directory", e))
    }

    /// Every name that belongs to a store is a plain file or absent, and the
    /// live store has no sidecar. Changes nothing.
    pub(super) fn check_removable(&self) -> Result<(), FilesError> {
        for name in [DB_FILE, TOMBSTONE].into_iter().chain(STAGED_NAMES) {
            self.require_plain(name)?;
        }
        for name in SIDECARS {
            if self.require_plain(name)? {
                return Err(FilesError::Sidecar(name));
            }
        }
        Ok(())
    }

    /// Whether the store, its settings, a tombstone or a staged store is
    /// here: there is history to own.
    pub(super) fn has_history(&self) -> Result<bool, FilesError> {
        for name in [DB_FILE, SETTINGS_FILE, TOMBSTONE, STAGED] {
            if self.plain(name)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A rollback journal is beside the store: a commit was cut off, and
    /// only a writable open of the store rolls it back.
    pub(super) fn journal_present(&self) -> Result<bool, FilesError> {
        self.require_plain(SIDECARS[0])
    }

    /// Opens (creating, 0600) the plain file `name` in this directory,
    /// refusing a link or anything that is not a regular file.
    pub(super) fn open_private(
        &self,
        name: &'static str,
    ) -> Result<std::os::fd::OwnedFd, FilesError> {
        use std::os::fd::FromRawFd;
        let c = cname(name);
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: valid fd and NUL-terminated name; the mode is passed as the
        // variadic argument `openat` reads when O_CREAT is set.
        let raw = unsafe { libc::openat(self.fd(), c.as_ptr(), flags, 0o600 as libc::c_uint) };
        if raw < 0 {
            let e = io::Error::last_os_error();
            return Err(if e.raw_os_error() == Some(libc::ELOOP) {
                FilesError::NotPlain(name)
            } else {
                FilesError::Io(name, e)
            });
        }
        // SAFETY: `raw` is a fresh descriptor nothing else owns.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        let meta = fs::File::from(owned.try_clone().map_err(|e| FilesError::Io(name, e))?)
            .metadata()
            .map_err(|e| FilesError::Io(name, e))?;
        if !meta.is_file() {
            return Err(FilesError::NotPlain(name));
        }
        Ok(owned)
    }

    /// Removes the store: `history.db` becomes the tombstone, then the
    /// tombstone is unlinked. Nothing else is touched.
    pub(super) fn remove_store(&self, observe: &dyn Fn(Step)) -> Result<(), FilesError> {
        self.check_removable()?;
        observe(Step::Begin);
        // A stale tombstone is ours and already deleted in all but name.
        if self.require_plain(TOMBSTONE)? {
            self.unlink(TOMBSTONE)?;
        }
        if self.require_plain(DB_FILE)? {
            self.rename(DB_FILE, TOMBSTONE)
                .map_err(|e| FilesError::Io(DB_FILE, e))?;
            // Reported before the sync: from the rename on, the store is gone.
            observe(Step::Renamed);
            self.sync()?;
        } else {
            observe(Step::Renamed);
        }
        if self.require_plain(TOMBSTONE)? {
            let shared = self.links(TOMBSTONE)? > 1;
            self.unlink(TOMBSTONE)?;
            self.sync()?;
            if shared {
                return Err(FilesError::Linked(TOMBSTONE));
            }
        }
        observe(Step::TombstoneRemoved);
        Ok(())
    }

    fn rename(&self, from: &'static str, to: &'static str) -> io::Result<()> {
        let (from, to) = (cname(from), cname(to));
        // SAFETY: valid fd, NUL-terminated names, both relative to that fd.
        let rc = unsafe { libc::renameat(self.fd(), from.as_ptr(), self.fd(), to.as_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Removes `settings.json` by its exact name (delete). Temp files of a
    /// save carry a random name and hold only settings; they are not named
    /// here.
    pub(super) fn remove_settings(&self) -> Result<(), FilesError> {
        if self.require_plain(SETTINGS_FILE)? {
            self.unlink(SETTINGS_FILE)?;
            self.sync()?;
        }
        Ok(())
    }

    /// Removes whatever of a staged store is there, by its exact names, and
    /// returns whether anything was, and a name another link still keeps. If
    /// one of the names is not a plain file nothing is removed.
    fn unlink_staged(&self) -> Result<(bool, Option<&'static str>), FilesError> {
        let mut present = Vec::new();
        for name in STAGED_NAMES {
            if self.require_plain(name)? {
                present.push(name);
            }
        }
        let mut linked = None;
        for name in &present {
            if self.links(name)? > 1 {
                linked = Some(*name);
            }
            self.unlink(name)?;
        }
        Ok((!present.is_empty(), linked))
    }

    /// What a crash during [`remove_store`](Self::remove_store) or a clear's
    /// staging left: the tombstone, the staged store, and, when there is no
    /// `history.db`, sidecars that belong to nothing. Creates nothing;
    /// removes only those exact names; a name that is not a plain file is
    /// left alone, and a staged one stops this before anything is removed.
    pub(super) fn finish_leftover(&self) -> Result<(), FilesError> {
        for name in STAGED_NAMES {
            self.require_plain(name)?;
        }
        let mut removed = false;
        let mut shared = None;
        if self.require_plain(TOMBSTONE)? {
            shared = (self.links(TOMBSTONE)? > 1).then_some(TOMBSTONE);
            self.unlink(TOMBSTONE)?;
            removed = true;
        }
        if !self.require_plain(DB_FILE)? {
            for name in SIDECARS {
                if self.require_plain(name)? {
                    self.unlink(name)?;
                    removed = true;
                }
            }
        }
        let (staged, linked) = self.unlink_staged()?;
        if removed || staged {
            self.sync()?;
        }
        match shared.or(linked) {
            Some(name) => Err(FilesError::Linked(name)),
            None => Ok(()),
        }
    }
}

/// Finishes what a crashed removal left, at every bridge start (history on
/// or off). Best effort: a directory that is absent, unsafe or unreadable is
/// the caller's own `unavailable`, and a failure is logged, not fatal.
pub(super) fn recover(location: &super::Location) {
    match open::dir_exists(location.dir()) {
        Ok(true) => {}
        _ => return,
    }
    let result = StoreDir::open(location.dir(), open::effective_uid()).and_then(|dir| {
        match dir.finish_leftover() {
            Ok(()) => {}
            // The tombstone is gone; the journal check must still run.
            Err(FilesError::Linked(name)) => {
                tracing::warn!(name, "another link keeps deleted history");
            }
            // A staged name is not the store's: the journal check still runs.
            Err(FilesError::NotPlain(name)) if STAGED_NAMES.contains(&name) => {
                tracing::warn!(name, "a staged history name is not a plain file");
            }
            Err(e) => return Err(e),
        }
        dir.journal_present()
    });
    match result {
        Ok(true) => roll_back_journal(location),
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, "history could not finish removing an old store"),
    }
}

/// A commit that was cut off while history was off leaves a hot journal that
/// nothing else would ever open the store to roll back, and a read-only open
/// (the preview, the reader) cannot. Under the writer lock, at start, open
/// the store once for write and let SQLite finish or discard the commit. A
/// store that is missing is left missing; whatever stays is refused by the
/// removal's own check.
fn roll_back_journal(location: &super::Location) {
    if !location.db_path().exists() {
        return;
    }
    let result = crate::sqlite_file::open_owner_only(&location.db_path())
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())
        });
    if let Err(error) = result {
        tracing::warn!(%error, "a leftover journal could not be rolled back");
    }
}
