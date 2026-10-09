//! The saved hit counts: one small JSON file in the bridge's state
//! directory (owner question N1: counts survive a bridge restart).
//!
//! Chosen over a SQLite store because it is one file with no schema and
//! nothing here needs queries. The file checks are the stores' own
//! ([`crate::sqlite_file::file_problem`], #90) plus the mode and size.
//!
//! **Reading** opens the file `O_NOFOLLOW | O_NONBLOCK` (a planted link or
//! FIFO can neither redirect nor hang the bridge), then checks the opened
//! file itself: a regular file, owned by the bridge's user, with no other
//! hard link, not writable by group or others, at most [`MAX_FILE_BYTES`].
//! The text must parse as a version this bridge reads and pass
//! [`validate`]. Anything else is an error, and the caller keeps the counts
//! in memory and leaves the file alone.
//!
//! **Versions.** Version 2 (N3, plan
//! `2026-10-09-n3-unused-window-from-daemon-counters.md`) adds the daemon's
//! counters at the last ping counted (`daemon`) and, from the shutdown save
//! only, `stoppedUnixMs`: what the next run judges a restart from
//! ([`crate::cache::rule_hits`]). Version 1 files still load, with neither,
//! and a file with neither is written as version 1, byte for byte the shape
//! a bridge from before N3 reads: so is every file of a bridge that doesn't
//! trust the daemon's counters (TCP, the shipped per-user setup), and a
//! rollback of it keeps its counts.
//! Fields this bridge doesn't know are ignored, so a later additive field
//! doesn't make an older bridge distrust the whole file; a higher version is
//! refused.
//!
//! **Writing** creates a temp file of its own (named for the process and a
//! random number, so two bridges on one state directory never share or
//! delete each other's) `O_CREAT | O_EXCL | O_NOFOLLOW` with mode 0600,
//! syncs it, renames it over the old file and syncs the directory, so a crash
//! leaves the old file or the new one (and, between the create and the
//! rename, a stray temp file nothing reads). The same [`validate`] that
//! guards reading guards writing, and the entry, name and time limits bound
//! the size (see [`MAX_FILE_BYTES`]): everything `save` writes, `load`
//! accepts.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cache::rule_hits::{
    keepable_name, DaemonBaseline, Saved, MAX_FUTURE_SKEW_MS, MAX_TRACKED_RULES,
};
use crate::state_file::invalid;
#[cfg(test)]
pub(crate) use crate::state_file::{temp_path, Facts};
use crate::ws_messages::RuleHitWire;

/// The largest file read or written. At the limits (10 000 entries, names of
/// 256 bytes, every byte a quote or backslash that JSON doubles, maximal
/// counts) a file is about 6 MB.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// The version written when there is a restart to judge.
const VERSION: u32 = 2;
/// The oldest version read.
const OLDEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileFormat {
    version: u32,
    since_unix_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_gap_unix_ms: Option<i64>,
    hits: Vec<RuleHitWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonBaseline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stopped_unix_ms: Option<i64>,
}

/// Reads the saved counts; `None` when there is no file.
pub fn load(path: &Path) -> io::Result<Option<Saved>> {
    let Some(bytes) = crate::state_file::read(path, MAX_FILE_BYTES)? else {
        return Ok(None);
    };
    let format: FileFormat = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("couldn't parse the file: {e}")))?;
    validate(&format, now_ms())?;
    Ok(Some(Saved {
        since_unix_ms: format.since_unix_ms,
        last_gap_unix_ms: format.last_gap_unix_ms,
        hits: format.hits,
        daemon: format.daemon,
        stopped_unix_ms: format.stopped_unix_ms,
    }))
}

/// Replaces the file with `saved`, atomically.
pub fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    let version = if saved.daemon.is_none() && saved.stopped_unix_ms.is_none() {
        OLDEST_VERSION
    } else {
        VERSION
    };
    let format = FileFormat {
        version,
        since_unix_ms: saved.since_unix_ms,
        last_gap_unix_ms: saved.last_gap_unix_ms,
        hits: saved.hits.clone(),
        daemon: saved.daemon,
        stopped_unix_ms: saved.stopped_unix_ms,
    };
    validate(&format, now_ms())?;
    let bytes = serde_json::to_vec(&format)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(invalid("the counts are too large to save"));
    }
    crate::state_file::write(path, &bytes)
}

#[cfg(test)]
fn check_facts(facts: &Facts, euid: u32) -> io::Result<()> {
    crate::state_file::check_facts(facts, euid, MAX_FILE_BYTES)
}

/// Whether a time is one this bridge could have recorded by `now_ms`.
fn plausible_time(unix_ms: i64, now_ms: i64) -> bool {
    (0..=now_ms.saturating_add(MAX_FUTURE_SKEW_MS)).contains(&unix_ms)
}

fn validate(format: &FileFormat, now_ms: i64) -> io::Result<()> {
    if !(OLDEST_VERSION..=VERSION).contains(&format.version) {
        return Err(invalid(format!("unsupported version {}", format.version)));
    }
    if !plausible_time(format.since_unix_ms, now_ms) {
        return Err(invalid("bad start time"));
    }
    if format
        .last_gap_unix_ms
        .is_some_and(|gap| !plausible_time(gap, now_ms))
    {
        return Err(invalid("bad time of the last gap"));
    }
    if format
        .daemon
        .is_some_and(|daemon| !plausible_time(daemon.ping_unix_ms, now_ms))
    {
        return Err(invalid("bad time of the last daemon ping"));
    }
    if format
        .stopped_unix_ms
        .is_some_and(|stopped| !plausible_time(stopped, now_ms))
    {
        return Err(invalid("bad stop time"));
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
        if !plausible_time(hit.last_hit_unix_ms, now_ms) {
            return Err(invalid(format!("bad last hit time for {:?}", hit.name)));
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "rule_hits_file/tests.rs"]
mod tests;
