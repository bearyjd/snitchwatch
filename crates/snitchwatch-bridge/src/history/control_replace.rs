//! #444: the staged store that replaces the old one in a clear. It is
//! created through the directory handle, filled and synced while the old
//! store is untouched, and takes the live name in one `renameat`, so a crash
//! or a failure leaves either the old store whole or the new one with what
//! it keeps. Where there was no store, the rename is one that never
//! replaces.

use std::fs;
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::{cname, FilesError, Step, StoreDir, SIDECARS, STAGED, STAGED_SIDECARS, TOMBSTONE};
use crate::history::DB_FILE;

/// Which file a name stood for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::history) struct FileId {
    dev: u64,
    ino: u64,
}

/// What became of the old store once the rename was attempted.
#[derive(Debug)]
pub(in crate::history) enum Replaced {
    /// The new store has the live name, durably. `linked`: another name
    /// still keeps the old one.
    Done { linked: bool },
    /// The new store has the live name, but the directory could not be
    /// synced: a power loss may bring the old one back.
    NotDurable { linked: bool, reason: String },
    /// Whether the rename happened cannot be told.
    Unknown(String),
}

#[cfg(test)]
fn injected(fault: Fault) -> bool {
    FAULT.get() == Some(fault)
}

#[cfg(test)]
fn injected_error() -> io::Error {
    io::Error::other("an injected failure")
}

fn identity(meta: &fs::Metadata) -> FileId {
    FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

impl StoreDir {
    /// Opens the plain file `name` without following a link or waiting on a
    /// FIFO.
    fn open_at(&self, name: &'static str, flags: libc::c_int) -> Result<fs::File, FilesError> {
        let c = cname(name);
        let flags = flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        // SAFETY: valid fd and NUL-terminated name; the mode is passed as the
        // variadic argument `openat` reads when O_CREAT is set.
        let raw = unsafe { libc::openat(self.fd(), c.as_ptr(), flags, 0o600 as libc::c_uint) };
        if raw < 0 {
            return Err(FilesError::Io(name, io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a fresh descriptor nothing else owns.
        Ok(unsafe { fs::File::from_raw_fd(raw) })
    }

    /// The plain file behind `name` and its link count; `None` when there is
    /// no such name.
    fn file_at(&self, name: &'static str) -> Result<Option<(FileId, u64)>, FilesError> {
        let Some(st) = self.stat(name)? else {
            return Ok(None);
        };
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(FilesError::NotPlain(name));
        }
        let id = FileId {
            dev: st.st_dev,
            ino: st.st_ino,
        };
        Ok(Some((id, st.st_nlink)))
    }

    /// Creates the empty staged file, 0600, after removing any leftover one.
    /// `O_EXCL`, so nothing already there is ever opened. The descriptor is
    /// closed before this returns: closing it once SQLite has the file open
    /// would drop SQLite's locks.
    pub(in crate::history) fn create_staged(&self) -> Result<FileId, FilesError> {
        if let (_, Some(name)) = self.unlink_staged()? {
            return Err(FilesError::StagedLinked(name));
        }
        let file = self.open_at(STAGED, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
        // Whatever the umask left.
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| FilesError::Io(STAGED, e))?;
        let meta = file.metadata().map_err(|e| FilesError::Io(STAGED, e))?;
        Ok(identity(&meta))
    }

    /// The staged name is still the file that was created, and its only name.
    pub(in crate::history) fn require_staged(&self, id: FileId) -> Result<(), FilesError> {
        match self.file_at(STAGED)? {
            Some((found, 1)) if found == id => Ok(()),
            _ => Err(FilesError::NotStaged(STAGED)),
        }
    }

    /// Syncs the staged file's data. SQLite must have closed it.
    pub(in crate::history) fn sync_staged(&self, id: FileId) -> Result<(), FilesError> {
        self.require_staged(id)?;
        let file = self.open_at(STAGED, libc::O_RDONLY)?;
        let meta = file.metadata().map_err(|e| FilesError::Io(STAGED, e))?;
        if identity(&meta) != id {
            return Err(FilesError::NotStaged(STAGED));
        }
        #[cfg(test)]
        if injected(Fault::StagedSync) {
            return Err(FilesError::Io(STAGED, injected_error()));
        }
        file.sync_all().map_err(|e| FilesError::Io(STAGED, e))
    }

    /// Removes a staged store that will not be used. Best effort: what stays
    /// is removed by the next start.
    pub(in crate::history) fn discard_staged(&self) {
        let removed = self.unlink_staged().and_then(|(any, _)| {
            if any {
                self.sync()?;
            }
            Ok(())
        });
        if let Err(error) = removed {
            tracing::warn!(%error, "a staged history store could not be removed");
        }
    }

    /// Renames the staged store over `history.db`, or, without `had_store`,
    /// onto that name only while nothing is there. An error means nothing was
    /// replaced; anything else is in the returned outcome.
    pub(in crate::history) fn replace_store(
        &self,
        id: FileId,
        had_store: bool,
        observe: &dyn Fn(Step),
    ) -> Result<Replaced, FilesError> {
        // Checked immediately before the rename: a journal left beside either
        // file would be applied to the new store and corrupt it.
        for name in SIDECARS.into_iter().chain(STAGED_SIDECARS) {
            if self.require_plain(name)? {
                return Err(FilesError::Sidecar(name));
            }
        }
        if self.plain(TOMBSTONE)?.is_some() {
            return Err(FilesError::Sidecar(TOMBSTONE));
        }
        let old = if had_store {
            Some(self.file_at(DB_FILE)?.ok_or(FilesError::Missing(DB_FILE))?)
        } else {
            None
        };
        self.require_staged(id)?;
        let linked = old.is_some_and(|(_, links)| links > 1);
        let old = old.map(|(old, _)| old);
        if let Err(error) = self.rename_staged(had_store) {
            // The name is taken, and what took it was not touched.
            if !had_store && error.kind() == io::ErrorKind::AlreadyExists {
                return Err(FilesError::Io(DB_FILE, error));
            }
            // A reported failure does not say what happened: look.
            match self.live_file() {
                Ok(Some(live)) if live == id => {}
                Ok(live) if live == old => return Err(FilesError::Io(DB_FILE, error)),
                Ok(_) => {
                    return Ok(Replaced::Unknown(format!(
                        "{error}; {DB_FILE} is neither the old store nor the new one"
                    )))
                }
                Err(look) => return Ok(Replaced::Unknown(format!("{error}; {look}"))),
            }
        }
        observe(Step::Replaced);
        Ok(match self.sync_replaced() {
            Ok(()) => Replaced::Done { linked },
            Err(e) => Replaced::NotDurable {
                linked,
                reason: e.to_string(),
            },
        })
    }

    fn rename_staged(&self, had_store: bool) -> io::Result<()> {
        #[cfg(test)]
        if injected(Fault::Rename) {
            return Err(injected_error());
        }
        let done = if had_store {
            self.rename(STAGED, DB_FILE)
        } else {
            self.rename_new(STAGED, DB_FILE)
        };
        #[cfg(test)]
        if injected(Fault::RenameReportedFailed) || injected(Fault::OutcomeUnknown) {
            return Err(injected_error());
        }
        done
    }

    /// A rename that never replaces: `EEXIST` if `to` is any entry at all.
    /// A file system without it refuses; there is no fallback.
    fn rename_new(&self, from: &'static str, to: &'static str) -> io::Result<()> {
        let (from, to) = (cname(from), cname(to));
        let (fd, flags) = (self.fd(), libc::RENAME_NOREPLACE);
        // SAFETY: valid fd, NUL-terminated names, both relative to that fd.
        let rc = unsafe { libc::renameat2(fd, from.as_ptr(), fd, to.as_ptr(), flags) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn live_file(&self) -> Result<Option<FileId>, FilesError> {
        #[cfg(test)]
        if injected(Fault::OutcomeUnknown) {
            return Err(FilesError::Io(DB_FILE, injected_error()));
        }
        Ok(self.file_at(DB_FILE)?.map(|(id, _)| id))
    }

    fn sync_replaced(&self) -> Result<(), FilesError> {
        #[cfg(test)]
        if injected(Fault::DirSync) {
            return Err(FilesError::Io("the history directory", injected_error()));
        }
        self.sync()
    }
}

/// Test seam: a failure the file system cannot be made to produce.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::history) enum Fault {
    /// Syncing the staged file fails.
    StagedSync,
    /// The rename fails and changes nothing.
    Rename,
    /// The rename happens but reports a failure.
    RenameReportedFailed,
    /// The rename reports a failure and the live name cannot be looked at.
    OutcomeUnknown,
    /// Syncing the directory after the rename fails.
    DirSync,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

/// Runs `run` on this thread with `fault` injected.
#[cfg(test)]
pub(in crate::history) fn with_fault<T>(fault: Fault, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAULT.set(None);
        }
    }
    let _reset = Reset;
    FAULT.set(Some(fault));
    run()
}
