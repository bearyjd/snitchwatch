//! A small owner-only state file in the bridge's state directory, read and
//! written safely. Moved out of `cache::rule_hits_file` (#94) so every JSON
//! state file (the hit counts, the curated defaults' choices) gets the same
//! checks. The file checks are the stores' own
//! ([`crate::sqlite_file::file_problem`], #90) plus the mode and size.
//!
//! **Reading** opens the file `O_NOFOLLOW | O_NONBLOCK` (a planted link or
//! FIFO can neither redirect nor hang the bridge), then checks the opened
//! file itself: a regular file, owned by the bridge's user, with no other
//! hard link, not writable by group or others, at most `max_bytes`.
//!
//! **Writing** creates a temp file of its own (named for the process and a
//! random number, so two bridges on one state directory never share or
//! delete each other's) `O_CREAT | O_EXCL | O_NOFOLLOW` with mode 0600,
//! syncs it, renames it over the old file and syncs the directory, so a crash
//! leaves the old file or the new one (and, between the create and the
//! rename, a stray temp file nothing reads).

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::sqlite_file::{file_problem, FileFacts};

const FILE_MODE: u32 = 0o600;

/// What `fstat` says about the opened file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Facts {
    pub(crate) is_file: bool,
    pub(crate) uid: u32,
    pub(crate) links: u64,
    pub(crate) mode: u32,
    pub(crate) len: u64,
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// The file's bytes; `None` when there is no file.
pub fn read(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    let facts = Facts {
        is_file: meta.is_file(),
        uid: meta.uid(),
        links: meta.nlink(),
        mode: meta.mode(),
        len: meta.len(),
    };
    check_facts(&facts, effective_uid(), max_bytes)?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid("the file is too large"));
    }
    Ok(Some(bytes))
}

/// Replaces the file with `bytes`, atomically. The caller checks the size.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(invalid("the file is not a regular file")),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let temp = temp_path(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temp)?;
    let written = file
        .set_permissions(fs::Permissions::from_mode(FILE_MODE))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(dir)?
        .sync_all()
}

/// A temp file name no other save uses: this process's id and a random
/// number.
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let unique = rand::random::<u64>();
    path.with_file_name(format!(".{name}.{}.{unique:016x}.tmp", std::process::id()))
}

pub(crate) fn check_facts(facts: &Facts, euid: u32, max_bytes: u64) -> io::Result<()> {
    let file = FileFacts {
        regular: facts.is_file,
        uid: facts.uid,
        links: facts.links,
    };
    if let Some(why) = file_problem(&file, euid) {
        return Err(invalid(format!("the file {why}")));
    }
    if facts.mode & 0o022 != 0 {
        return Err(invalid("the file is writable by other users"));
    }
    if facts.len > max_bytes {
        return Err(invalid("the file is too large"));
    }
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}
