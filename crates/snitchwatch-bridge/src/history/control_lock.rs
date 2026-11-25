//! PR 10 (#418): one bridge records into a state directory at a time.
//!
//! `flock(LOCK_EX | LOCK_NB)` on a **separate** `history.lock`, never on the
//! database: SQLite's own POSIX record locks drop when any descriptor of
//! their inode closes, and `flock` belongs to the open file description, so
//! the two never interfere. The lock is taken before the first writable
//! open, held for as long as the owner lives, and released by the kernel at
//! exit, `kill -9` included. A reader does not take it.

use std::os::fd::{AsRawFd, OwnedFd};

use super::control_files::{FilesError, StoreDir};

/// The lock file's exact name. Never removed by clear or delete.
pub(super) const LOCK_FILE: &str = "history.lock";

#[derive(Debug, thiserror::Error)]
pub(super) enum LockError {
    #[error("history is in use by another bridge")]
    InUse,
    #[error("{0}")]
    Files(#[from] FilesError),
    #[error("history.lock: {0}")]
    Io(std::io::Error),
}

/// Holds the lock until dropped.
#[derive(Debug)]
pub(super) struct WriterLock {
    _file: OwnedFd,
}

impl StoreDir {
    /// Takes the writer lock, creating `history.lock` (0600) beside the
    /// store if it is missing. The directory must already exist.
    pub(super) fn lock_writer(&self) -> Result<WriterLock, LockError> {
        let file = self.open_private(LOCK_FILE).map_err(|e| match e {
            FilesError::Io(_, io) => LockError::Io(io),
            other => LockError::Files(other),
        })?;
        // SAFETY: a valid descriptor we own; `flock` touches no memory.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            return Err(if e.kind() == std::io::ErrorKind::WouldBlock {
                LockError::InUse
            } else {
                LockError::Io(e)
            });
        }
        Ok(WriterLock { _file: file })
    }
}
