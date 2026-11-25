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
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FilesError {
    #[error("{0}")]
    Directory(Unavailable),
    /// An entry that is not a plain file: left exactly as it was.
    #[error("{0} is not a plain file; nothing was removed")]
    NotPlain(&'static str),
    /// A sidecar of a store that is supposed to be closed: a commit did not
    /// finish, so the next start's own recovery must see it.
    #[error("{0} is present; nothing was removed")]
    Sidecar(&'static str),
    #[error("{0}: {1}")]
    Io(&'static str, io::Error),
}

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

    /// `Some(is_plain_file)` when `name` exists (judged without following a
    /// link), `None` when it does not.
    fn plain(&self, name: &'static str) -> Result<Option<bool>, FilesError> {
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
        let mode = unsafe { st.assume_init() }.st_mode;
        Ok(Some(mode & libc::S_IFMT == libc::S_IFREG))
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

    /// Every name that belongs to a store is a plain file or absent, and no
    /// sidecar exists. Changes nothing.
    pub(super) fn check_removable(&self) -> Result<(), FilesError> {
        for name in [DB_FILE, TOMBSTONE] {
            self.require_plain(name)?;
        }
        for name in SIDECARS {
            if self.require_plain(name)? {
                return Err(FilesError::Sidecar(name));
            }
        }
        Ok(())
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
            self.rename_to_tombstone()?;
            self.sync()?;
        }
        observe(Step::Renamed);
        if self.require_plain(TOMBSTONE)? {
            self.unlink(TOMBSTONE)?;
            self.sync()?;
        }
        observe(Step::TombstoneRemoved);
        Ok(())
    }

    fn rename_to_tombstone(&self) -> Result<(), FilesError> {
        let (from, to) = (cname(DB_FILE), cname(TOMBSTONE));
        // SAFETY: valid fd, NUL-terminated names, both relative to that fd.
        let rc = unsafe { libc::renameat(self.fd(), from.as_ptr(), self.fd(), to.as_ptr()) };
        if rc != 0 {
            return Err(FilesError::Io(DB_FILE, io::Error::last_os_error()));
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

    /// What a crash during [`remove_store`](Self::remove_store) left: the
    /// tombstone, and, when there is no `history.db`, sidecars that belong to
    /// nothing. Creates nothing; removes only those exact names; a name that
    /// is not a plain file is left alone.
    pub(super) fn finish_leftover(&self) -> Result<(), FilesError> {
        let mut removed = false;
        if self.require_plain(TOMBSTONE)? {
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
        if removed {
            self.sync()?;
        }
        Ok(())
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
    let result =
        StoreDir::open(location.dir(), open::effective_uid()).and_then(|dir| dir.finish_leftover());
    if let Err(error) = result {
        tracing::warn!(%error, "history could not finish removing an old store");
    }
}
