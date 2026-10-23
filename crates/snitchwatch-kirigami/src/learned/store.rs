//! The file `learned.json` (phase L2): how it is read, merged and written.
//!
//! * Written with `private_files::write_private` (0600, atomic replace).
//! * Read-merge-write under an advisory `flock` on a SEPARATE lock file
//!   (`write_private` renames over the target, so the target cannot carry the
//!   lock). If locking is impossible (a directory that does not support it) or
//!   stays contended, the write goes ahead without it (last writer wins) and
//!   the report says so.
//! * A missing file starts empty. A file that is not ours (another owner, a
//!   symlink, a hard link, not a regular file) is neither read nor replaced. A
//!   file older than 30 days is discarded. A file that cannot be understood
//!   (damaged, too big, another version, or written by a NEWER one) is never
//!   destroyed: it is renamed to `learned.json.corrupt` before a new one is
//!   started, and the notice says so. Nothing here panics.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::aggregate::{Aggregate, BYTE_BUDGET, FILE_OVERHEAD};
use crate::logging::{open_no_follow, refuse_unless_ours};
use crate::private_files::{ensure_private_dir, write_private};

/// A file older than this is discarded.
pub const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// The file format. Version 2 keys programs by `path:` / `name:`; an older
/// file is kept aside, not migrated, and a newer one is never overwritten.
const VERSION: u32 = 2;

/// Larger than this is not read. The aggregate never serialises to more than
/// `BYTE_BUDGET + FILE_OVERHEAD` (it keeps an upper estimate of its size and
/// holds it under the budget; see `aggregate`), so a file this recorder wrote
/// is always accepted, with a wide margin.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const _: () = assert!(MAX_FILE_BYTES >= 3 * (BYTE_BUDGET + FILE_OVERHEAD) as u64);

/// Where a file that cannot be used is kept.
const ASIDE_SUFFIX: &str = ".corrupt";

/// Just the version, read first so a newer file is recognised whatever else
/// it holds.
#[derive(Deserialize)]
struct Header {
    version: u32,
}

/// What is in the file.
#[derive(Serialize, Deserialize)]
struct FileBody {
    version: u32,
    saved_ms: i64,
    aggregate: Aggregate,
}

/// What the owner is told about the file. Fixed texts: nothing external is in
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The file could not be used (damaged, too big or an older format); it
    /// was kept aside as `learned.json.corrupt` and a new one started.
    Corrupt,
    /// The file was written by a newer Snitchwatch; it was kept aside, not
    /// overwritten.
    Newer,
    /// The file is not a plain file of ours; it was not used.
    Untrusted,
    /// The file could not be read.
    Unreadable,
    /// The file could not be written.
    WriteFailed,
    /// The file was older than 30 days and was discarded.
    Expired,
}

impl Notice {
    pub fn text(self) -> &'static str {
        match self {
            Notice::Corrupt => "The saved list of what was seen could not be used. It was kept as learned.json.corrupt and a new list was started.",
            Notice::Newer => "The saved list of what was seen was written by a newer Snitchwatch. It was kept as learned.json.corrupt, not overwritten, and a new list was started.",
            Notice::Untrusted => "The saved list of what was seen is not a plain file of yours, so it was not used or changed.",
            Notice::Unreadable => "The saved list of what was seen could not be read.",
            Notice::WriteFailed => "What was seen could not be saved to disk; it is only kept while this window is open.",
            Notice::Expired => "The saved list of what was seen was older than 30 days and was discarded.",
        }
    }
}

/// What a read found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    pub aggregate: Aggregate,
    pub notice: Option<Notice>,
}

/// What a write did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteReport {
    /// The lock was not obtained: the write went ahead without it.
    pub lock_degraded: bool,
    /// What the file looked like before this write, if it was not usable.
    pub notice: Option<Notice>,
}

/// Where the summary is kept; the file in production, a double in tests.
pub trait Backend: Send + Sync {
    fn load(&self, now_ms: i64) -> Loaded;
    /// Merge `delta` into what is stored and store the result.
    fn merge_write(&self, delta: &Aggregate, now_ms: i64) -> Result<WriteReport, Notice>;
    /// Delete what is stored.
    fn clear(&self) -> Result<(), Notice>;
}

/// How hard to try for the lock before writing without it.
#[derive(Debug, Clone, Copy)]
pub struct LockPolicy {
    pub attempts: u32,
    pub pause: Duration,
}

impl Default for LockPolicy {
    fn default() -> Self {
        Self {
            attempts: 20,
            pause: Duration::from_millis(25),
        }
    }
}

/// The real file.
pub struct FileBackend {
    path: PathBuf,
    policy: LockPolicy,
    max_bytes: u64,
}

impl FileBackend {
    pub fn new(path: PathBuf) -> Self {
        Self::with_policy(path, LockPolicy::default())
    }

    pub fn with_policy(path: PathBuf, policy: LockPolicy) -> Self {
        Self {
            path,
            policy,
            max_bytes: MAX_FILE_BYTES,
        }
    }

    /// The same backend refusing files larger than `max_bytes` (for tests).
    #[cfg(test)]
    pub fn with_max_bytes(self, max_bytes: u64) -> Self {
        Self { max_bytes, ..self }
    }

    /// `<file>.lock`.
    pub fn lock_path(&self) -> PathBuf {
        lock_path_for(&self.path)
    }
}

pub fn lock_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// What reading the file found.
enum Found {
    Missing,
    Usable(Aggregate),
    Unusable(Notice),
}

fn read_file(path: &Path, now_ms: i64, max_bytes: u64) -> Found {
    let mut file = match open_no_follow(path, std::fs::OpenOptions::new().read(true)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Found::Missing,
        // A symlink in the way (O_NOFOLLOW) is as untrusted as a foreign file.
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Found::Unusable(Notice::Untrusted)
        }
        Err(_) => return Found::Unusable(Notice::Unreadable),
    };
    if refuse_unless_ours(path, &file).is_err() {
        return Found::Unusable(Notice::Untrusted);
    }
    match file.metadata() {
        Ok(meta) if meta.len() <= max_bytes => {}
        Ok(_) => return Found::Unusable(Notice::Corrupt),
        Err(_) => return Found::Unusable(Notice::Unreadable),
    }
    let mut text = String::new();
    // Bounded again, in case the file grows while it is read.
    if (&mut file)
        .take(max_bytes + 1)
        .read_to_string(&mut text)
        .is_err()
    {
        return Found::Unusable(Notice::Corrupt);
    }
    if text.len() as u64 > max_bytes {
        return Found::Unusable(Notice::Corrupt);
    }
    match serde_json::from_str::<Header>(&text) {
        Ok(header) if header.version > VERSION => return Found::Unusable(Notice::Newer),
        Ok(header) if header.version == VERSION => {}
        _ => return Found::Unusable(Notice::Corrupt),
    }
    let Ok(body) = serde_json::from_str::<FileBody>(&text) else {
        return Found::Unusable(Notice::Corrupt);
    };
    let created = body.aggregate.created_ms;
    if created != 0 && now_ms.saturating_sub(created) > RETENTION_MS {
        return Found::Unusable(Notice::Expired);
    }
    // Through the aggregator, so every text is made safe and every cap holds
    // whatever the file says.
    Found::Usable(Aggregate::default().merge(body.aggregate))
}

/// Delete the file when it is older than the retention period (a startup
/// housekeeping step, so an old list does not sit on disk for ever). A file
/// that cannot be understood is left for the owner to look at.
pub fn discard_if_stale(path: &Path, now_ms: i64) -> bool {
    matches!(
        read_file(path, now_ms, MAX_FILE_BYTES),
        Found::Unusable(Notice::Expired)
    ) && std::fs::remove_file(path).is_ok()
}

/// Atomically rename `path` to `<path>.corrupt` (replacing an older one).
fn move_aside(path: &Path) -> io::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(ASIDE_SUFFIX);
    std::fs::rename(path, path.with_file_name(name))
}

/// One non-blocking `flock`.
fn try_flock(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` owns a valid open file descriptor for the whole call, and
    // `flock` reads and writes no memory of ours.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl FileBackend {
    /// The lock, if it can be had: the open lock file (dropping it releases
    /// the lock) and whether the lock was given up on. Contention is retried
    /// per the policy; a lock that cannot be opened or is not supported by the
    /// directory is given up on at once. Never fails: the caller goes on
    /// without it (last writer wins).
    fn acquire(&self) -> (Option<File>, bool) {
        let lock_path = self.lock_path();
        let opened = open_no_follow(
            &lock_path,
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .mode(0o600),
        );
        let Ok(file) = opened else {
            return (None, true);
        };
        if refuse_unless_ours(&lock_path, &file).is_err() {
            return (None, true);
        }
        let attempts = self.policy.attempts.max(1);
        for attempt in 0..attempts {
            match try_flock(&file) {
                Ok(()) => return (Some(file), false),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    if attempt + 1 < attempts {
                        std::thread::sleep(self.policy.pause);
                    }
                }
                // Not supported here (or any other failure): do not wait.
                Err(_) => return (None, true),
            }
        }
        (None, true)
    }
}

impl Backend for FileBackend {
    fn load(&self, now_ms: i64) -> Loaded {
        match read_file(&self.path, now_ms, self.max_bytes) {
            Found::Missing => Loaded::default(),
            Found::Usable(aggregate) => Loaded {
                aggregate,
                notice: None,
            },
            Found::Unusable(notice) => Loaded {
                aggregate: Aggregate::default(),
                notice: Some(notice),
            },
        }
    }

    fn merge_write(&self, delta: &Aggregate, now_ms: i64) -> Result<WriteReport, Notice> {
        if let Some(dir) = self.path.parent() {
            ensure_private_dir(dir).map_err(|_| Notice::WriteFailed)?;
        }
        let (_lock, lock_degraded) = self.acquire();
        let (stored, notice) = match read_file(&self.path, now_ms, self.max_bytes) {
            Found::Missing => (Aggregate::default(), None),
            Found::Usable(aggregate) => (aggregate, None),
            // A file that is not ours is neither read nor replaced.
            Found::Unusable(refused @ (Notice::Untrusted | Notice::Unreadable)) => {
                return Err(refused)
            }
            Found::Unusable(unusable) => {
                if matches!(unusable, Notice::Corrupt | Notice::Newer) {
                    // Never destroy what cannot be understood: keep it aside
                    // first, and write nothing if that is not possible.
                    move_aside(&self.path).map_err(|_| Notice::WriteFailed)?;
                }
                (Aggregate::default(), Some(unusable))
            }
        };
        let body = FileBody {
            version: VERSION,
            saved_ms: now_ms,
            aggregate: stored.merge(delta.clone()),
        };
        let bytes = serde_json::to_vec(&body).map_err(|_| Notice::WriteFailed)?;
        write_private(&self.path, &bytes).map_err(|_| Notice::WriteFailed)?;
        Ok(WriteReport {
            lock_degraded,
            notice,
        })
    }

    fn clear(&self) -> Result<(), Notice> {
        let (_lock, _) = self.acquire();
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Notice::WriteFailed),
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
