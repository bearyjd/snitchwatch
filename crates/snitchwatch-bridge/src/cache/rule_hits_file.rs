//! The saved hit counts: one small JSON file in the bridge's state
//! directory (owner question N1: counts survive a bridge restart).
//!
//! Chosen over a SQLite store because it is one file with no schema, and the
//! hardened open/replace is short enough to audit: PR #90's shared
//! `sqlite_file::open_owner_only` is not on this branch's base, and nothing
//! here needs queries.
//!
//! **Reading** opens the file `O_NOFOLLOW | O_NONBLOCK` (a planted link or
//! FIFO can neither redirect nor hang the bridge), then checks the opened
//! file itself: a regular file, owned by the bridge's user, not writable by
//! group or others, at most [`MAX_FILE_BYTES`]. The text must parse as the
//! current version and pass [`validate`]. Anything else is an error, and the
//! caller keeps the counts in memory and leaves the file alone.
//!
//! **Writing** removes any stale temp file (`remove_file` never follows a
//! link), creates a new one `O_CREAT | O_EXCL | O_NOFOLLOW` with mode 0600,
//! syncs it, renames it over the old file and syncs the directory, so a crash
//! leaves the old file or the new one. The same [`validate`] that guards
//! reading guards writing, and the entry and name limits bound the size
//! (see [`MAX_FILE_BYTES`]): everything `save` writes, `load` accepts.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache::rule_hits::{keepable_name, Saved, MAX_TRACKED_RULES};
use crate::ws_messages::RuleHitWire;

/// The largest file read or written. At the limits (10 000 entries, names of
/// 256 bytes, every byte a quote or backslash that JSON doubles, maximal
/// numbers) a file is about 6.3 MB.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const FILE_MODE: u32 = 0o600;
const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileFormat {
    version: u32,
    since_unix_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_gap_unix_ms: Option<i64>,
    hits: Vec<RuleHitWire>,
}

/// What `fstat` says about the opened file.
#[derive(Debug, Clone, Copy)]
struct Facts {
    is_file: bool,
    uid: u32,
    mode: u32,
    len: u64,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Reads the saved counts; `None` when there is no file.
pub fn load(path: &Path) -> io::Result<Option<Saved>> {
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
        mode: meta.mode(),
        len: meta.len(),
    };
    check_facts(&facts, effective_uid())?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("the file is too large"));
    }
    let format: FileFormat = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("couldn't parse the file: {e}")))?;
    validate(&format)?;
    Ok(Some(Saved {
        since_unix_ms: format.since_unix_ms,
        last_gap_unix_ms: format.last_gap_unix_ms,
        hits: format.hits,
    }))
}

/// Replaces the file with `saved`, atomically.
pub fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    let format = FileFormat {
        version: VERSION,
        since_unix_ms: saved.since_unix_ms,
        last_gap_unix_ms: saved.last_gap_unix_ms,
        hits: saved.hits.clone(),
    };
    validate(&format)?;
    let bytes = serde_json::to_vec(&format)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("the counts are too large to save"));
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(invalid("the file is not a regular file")),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let temp = temp_path(path);
    match fs::remove_file(&temp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temp)?;
    let written = file
        .set_permissions(fs::Permissions::from_mode(FILE_MODE))
        .and_then(|()| file.write_all(&bytes))
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

fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.tmp"))
}

fn check_facts(facts: &Facts, euid: u32) -> io::Result<()> {
    if !facts.is_file {
        return Err(invalid("the file is not a regular file"));
    }
    if facts.uid != euid {
        return Err(invalid("the file is not owned by this user"));
    }
    if facts.mode & 0o022 != 0 {
        return Err(invalid("the file is writable by other users"));
    }
    if facts.len > MAX_FILE_BYTES {
        return Err(invalid("the file is too large"));
    }
    Ok(())
}

fn validate(format: &FileFormat) -> io::Result<()> {
    if format.version != VERSION {
        return Err(invalid(format!("unsupported version {}", format.version)));
    }
    if format.since_unix_ms < 0 {
        return Err(invalid("bad start time"));
    }
    if format.hits.len() > MAX_TRACKED_RULES {
        return Err(invalid(format!(
            "too many entries ({}, at most {MAX_TRACKED_RULES})",
            format.hits.len()
        )));
    }
    let mut seen = HashSet::with_capacity(format.hits.len());
    for hit in &format.hits {
        if !keepable_name(&hit.name) {
            return Err(invalid("an entry has an invalid rule name"));
        }
        if !seen.insert(hit.name.as_str()) {
            return Err(invalid(format!("lists the rule {:?} twice", hit.name)));
        }
    }
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and always
    // succeeds.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
#[path = "rule_hits_file/tests.rs"]
mod tests;
