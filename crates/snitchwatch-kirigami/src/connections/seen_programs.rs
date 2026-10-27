//! First-seen state for the exception-first default (0c; plan
//! `docs/superpowers/plans/2026-10-27-exception-first-default.md`, step 4).
//!
//! Two separate things:
//!   * a persisted, capped set of executable paths seen before, with when
//!     each was last seen (least recently seen evicted past [`MAX_SEEN`]),
//!     merged with the file on every write because two GUIs can run;
//!   * an in-memory "new this session until acknowledged" set: after the
//!     baseline, a path not in the seen set is new until the user marks it
//!     seen. Only the baseline and that acknowledgement add paths to the
//!     persisted set, so an unacknowledged program stays new after a restart.
//!
//! The baseline is taken once per model life, at the first rules message
//! applied after a `ClearConnectionRows` has been applied (the bridge's
//! snapshot order is clear, row replay, rules). Only when the file could not
//! be used (missing, corrupt, another user's) are the programs present then
//! recorded as seen, and that always shows [`Self::notice`]: tampering with
//! the file can never quietly baseline a program. A reconnect resets nothing.

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::logging::open_no_follow;

/// The file under `paths::config_dir()`.
pub const SEEN_FILE_NAME: &str = "seen-programs.json";
/// At most this many paths are kept (least recently seen evicted).
pub const MAX_SEEN: usize = 5_000;
const VERSION: u32 = 1;
/// 5,000 paths of up to 4 KiB each would be ~20 MiB; real paths are short,
/// so anything larger than this is not a file this GUI wrote.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

const BASELINE_TEXT: &str = "Baseline recorded: programs seen so far will not be flagged as new; \
                             a program already running at that moment was baselined.";

#[derive(Debug, Serialize, Deserialize)]
struct FileBody {
    version: u32,
    programs: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    path: String,
    last_seen_ms: i64,
}

/// What reading the seen file found.
#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    Valid(HashMap<String, i64>),
    Missing,
    /// Not JSON this GUI wrote, or too large.
    Corrupt,
    /// Another user's file, a symlink, or a hard-linked or special file.
    Foreign,
    Unreadable,
}

impl Loaded {
    /// Why a file that is there was not used, for the notice; `None` for a
    /// valid or missing file.
    fn problem(&self) -> Option<&'static str> {
        match self {
            Loaded::Valid(_) | Loaded::Missing => None,
            Loaded::Corrupt => Some("it is damaged"),
            Loaded::Foreign => Some("it is not a plain file of yours"),
            Loaded::Unreadable => Some("it could not be opened"),
        }
    }
}

/// Read `path`, trusting it only if it is a regular file owned by `uid`
/// with one link.
pub fn read(path: &Path, uid: u32) -> Loaded {
    let mut file = match open_no_follow(path, std::fs::OpenOptions::new().read(true)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Loaded::Foreign,
        Err(_) => return Loaded::Unreadable,
    };
    let Ok(meta) = file.metadata() else {
        return Loaded::Unreadable;
    };
    if !meta.is_file() || meta.uid() != uid || meta.nlink() != 1 {
        return Loaded::Foreign;
    }
    let mut text = String::new();
    if (&mut file)
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .is_err()
        || text.len() as u64 > MAX_BYTES
    {
        return Loaded::Corrupt;
    }
    match serde_json::from_str::<FileBody>(&text) {
        Ok(body) if body.version == VERSION => Loaded::Valid(
            capped(body.programs.into_iter().map(|e| (e.path, e.last_seen_ms))).collect(),
        ),
        _ => Loaded::Corrupt,
    }
}

/// The [`MAX_SEEN`] most recently seen entries (ties by path), newest first.
fn capped(entries: impl IntoIterator<Item = (String, i64)>) -> impl Iterator<Item = (String, i64)> {
    let mut all: Vec<(String, i64)> = entries.into_iter().collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    all.dedup_by(|later, earlier| later.0 == earlier.0);
    all.into_iter().take(MAX_SEEN)
}

/// Merge `ours` with what `path` holds now (the later sighting wins), cap
/// it, and write it atomically with owner-only permissions. Returns the
/// merged set. A file that is not valid is replaced by `ours`.
pub fn write_merged(
    path: &Path,
    ours: &HashMap<String, i64>,
    uid: u32,
) -> io::Result<HashMap<String, i64>> {
    let mut merged = match read(path, uid) {
        Loaded::Valid(theirs) => theirs,
        _ => HashMap::new(),
    };
    for (path, &last) in ours {
        let slot = merged.entry(path.clone()).or_insert(last);
        *slot = (*slot).max(last);
    }
    let programs: Vec<Entry> = capped(merged)
        .map(|(path, last_seen_ms)| Entry { path, last_seen_ms })
        .collect();
    let body = serde_json::to_vec_pretty(&FileBody {
        version: VERSION,
        programs,
    })
    .map_err(io::Error::other)?;
    crate::private_files::write_private(path, &body)?;
    let FileBody { programs, .. } = serde_json::from_slice(&body).map_err(io::Error::other)?;
    Ok(programs
        .into_iter()
        .map(|e| (e.path, e.last_seen_ms))
        .collect())
}

/// Where the seen set lives.
#[derive(Debug, Clone)]
enum Backing {
    /// Never read or written (headless tests, demo mode): starts empty and
    /// baselines with the notice, like a first run.
    Memory,
    File {
        path: PathBuf,
        uid: u32,
    },
}

/// The first-seen state of one Connections model.
#[derive(Debug)]
pub struct SeenPrograms {
    backing: Backing,
    seen: HashMap<String, i64>,
    new_this_session: BTreeSet<String>,
    /// Why the load re-baselines (`Some("")` for a missing file or memory);
    /// `None` when a valid file was read.
    rebaseline: Option<&'static str>,
    clear_applied: bool,
    baselined: bool,
    notice: String,
}

impl Default for SeenPrograms {
    fn default() -> Self {
        Self::from_loaded(Backing::Memory, Loaded::Missing)
    }
}

impl SeenPrograms {
    /// Backed by the file at `path`, trusted only if owned by `uid`; read now.
    pub fn persisted(path: PathBuf, uid: u32) -> Self {
        let loaded = read(&path, uid);
        Self::from_loaded(Backing::File { path, uid }, loaded)
    }

    fn from_loaded(backing: Backing, loaded: Loaded) -> Self {
        let rebaseline = match &loaded {
            Loaded::Valid(_) => None,
            other => Some(other.problem().unwrap_or("")),
        };
        let seen = match loaded {
            Loaded::Valid(seen) => seen,
            _ => HashMap::new(),
        };
        Self {
            backing,
            seen,
            new_this_session: BTreeSet::new(),
            rebaseline,
            clear_applied: false,
            baselined: false,
            notice: String::new(),
        }
    }

    /// A `ClearConnectionRows` reached the row store.
    pub fn note_clear_applied(&mut self) {
        self.clear_applied = true;
    }

    pub fn is_baselined(&self) -> bool {
        self.baselined
    }

    /// A rules message was applied. The first one after a clear takes the
    /// baseline over `present` (the paths of every row the store holds).
    /// Returns whether the seen set or the new flags changed.
    pub fn on_rules_applied<'a>(
        &mut self,
        present: impl IntoIterator<Item = &'a str>,
        now_ms: i64,
    ) -> bool {
        if self.baselined || !self.clear_applied {
            return false;
        }
        self.baselined = true;
        let Some(problem) = self.rebaseline else {
            // A valid file: what was running is checked like any later row.
            self.observe(present, now_ms);
            return true;
        };
        for path in present {
            self.seen.insert(path.to_string(), now_ms);
        }
        self.notice = if problem.is_empty() {
            BASELINE_TEXT.to_string()
        } else {
            format!(
                "The list of programs seen before was not used ({problem}), so a new baseline \
                 was recorded. {BASELINE_TEXT}"
            )
        };
        self.persist();
        true
    }

    /// Rows arrived or changed: after the baseline, flag paths never seen
    /// and note when seen ones were last seen. Returns whether a path was
    /// newly flagged.
    pub fn observe<'a>(&mut self, paths: impl IntoIterator<Item = &'a str>, now_ms: i64) -> bool {
        if !self.baselined {
            return false;
        }
        let mut flagged = false;
        for path in paths {
            if let Some(last) = self.seen.get_mut(path) {
                *last = (*last).max(now_ms);
            } else if !self.new_this_session.contains(path) {
                self.new_this_session.insert(path.to_string());
                flagged = true;
            }
        }
        flagged
    }

    /// The user marked this session's new programs as seen. Returns whether
    /// anything changed.
    pub fn acknowledge(&mut self, now_ms: i64) -> bool {
        if self.new_this_session.is_empty() {
            return false;
        }
        for path in std::mem::take(&mut self.new_this_session) {
            self.seen.insert(path, now_ms);
        }
        self.persist();
        true
    }

    fn persist(&mut self) {
        let Backing::File { path, uid } = &self.backing else {
            self.seen = capped(std::mem::take(&mut self.seen)).collect();
            return;
        };
        match write_merged(path, &self.seen, *uid) {
            Ok(merged) => self.seen = merged,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "seen programs: not saved");
                self.seen = capped(std::mem::take(&mut self.seen)).collect();
            }
        }
    }

    /// Paths seen before. Never one that is new this session: a path is
    /// flagged only when it is not in the set, and only the acknowledgement,
    /// which clears every flag, adds a flagged path (a merge with the file
    /// happens only at the baseline, before any flag, and at that moment).
    pub fn seen_paths(&self) -> impl Iterator<Item = &str> {
        self.seen.keys().map(String::as_str)
    }

    pub fn is_new(&self, path: &str) -> bool {
        self.new_this_session.contains(path)
    }

    pub fn new_count(&self) -> usize {
        self.new_this_session.len()
    }

    /// The one-time baseline notice; empty once dismissed or before one.
    pub fn notice(&self) -> &str {
        &self.notice
    }

    pub fn dismiss_notice(&mut self) {
        self.notice.clear();
    }
}

#[cfg(test)]
mod tests;
