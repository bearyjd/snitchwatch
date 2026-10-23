//! What learning mode saw, as a bounded, mergeable summary (phase L2).
//!
//! One [`Program`] summarises every connection of one program (its path, else
//! its raw name): first and last seen, how many connections, which
//! destinations (the first 64 kept, the rest only counted), which ports
//! (capped), and how many went to a bare IP or an uncommon port. Programs are
//! kept in four classes, recomputed on every update, with flag precedence
//! A > B > D > C:
//!
//! * A: any bare-IP or uncommon-port connection. Cap 700, NEVER evicted; a new
//!   candidate is REFUSED when A is full.
//! * B: runs from a user-writable or temporary directory (and has no network
//!   flag). Keyed per PARENT DIRECTORY (one [`DirEntry`] with its programs), so
//!   junk paths in one directory collapse into one entry. Cap 500; oldest
//!   last-seen is evicted first.
//! * D: no path (a raw `comm` name that a program chooses itself). Cap 200;
//!   oldest last-seen first.
//! * C: a path, no flag. Cap 600; least active first.
//!
//! Each class evicts only within itself. An entry that becomes network-flagged
//! while A is full STAYS in its old class and is evicted last there. Nothing
//! is dropped silently: the last 50 refused or evicted keys are kept in a ring
//! with counts, and `dropped` counts them.
//!
//! BYTES, not just counts, are bounded. The count caps alone allow far more
//! than a file may hold (5,500 programs, each up to `MAX_PROGRAM_BYTES`, about
//! 200 MB), so the aggregate also keeps an upper estimate of its serialised
//! size and never lets it pass `BYTE_BUDGET`: before an update that could pass
//! it, unflagged entries are evicted (directories, then names, then paths, as
//! for the caps); if that is not enough a new program is refused and a known
//! one only has its counters updated. Every serialised file is therefore at
//! most `BYTE_BUDGET + FILE_OVERHEAD`, which is far below what the store
//! accepts (`store::MAX_FILE_BYTES`, asserted there).
//!
//! The summary merges (sum counts, earliest first, latest last, union of the
//! capped sets, caps re-applied), because the file on disk is shared by every
//! recording GUI. Every text kept comes from outside: the raw path is the exact
//! key (a rule needs it) and is NEVER shown; `display` is the sanitised copy
//! that may be.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::classify;
pub use super::program::*;
use super::program::{cut, MAX_PATH_BYTES};
use crate::app_names::sanitize_display;

pub const MAX_DIR_PROGRAMS: usize = 8;
pub const CAP_A: usize = 700;
pub const CAP_B: usize = 500;
pub const CAP_C: usize = 600;
pub const CAP_D: usize = 200;
pub const RING_LEN: usize = 50;

/// The most serialised bytes the programs and directories may take together.
pub const BYTE_BUDGET: usize = 16 * 1024 * 1024;
/// What one directory entry is charged: its field names, numbers, and its
/// path and display (and the path again as the map key), at their limits.
pub const MAX_DIR_BYTES: usize = 512 + 2 * (2 * MAX_PATH_BYTES + 4) + (2 * MAX_DISPLAY_BYTES + 4);
/// The header, the drop ring (50 entries of at most about 700 bytes) and the
/// counters, on top of the budget.
pub const FILE_OVERHEAD: usize = 64 * 1024;

/// The four classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Class {
    A,
    B,
    C,
    D,
}

/// What happened to a program that was handed to the aggregator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absorbed {
    Recorded,
    Refused,
}

/// Why a key is in the drop ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DropKind {
    Refused,
    Evicted,
}

/// One refused or evicted key. Shown, so `display` is sanitised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub display: String,
    pub kind: DropKind,
    pub class: Class,
    pub connections: u64,
    /// Whether it carried a network flag.
    pub flagged: bool,
}

impl Dropped {
    /// The same entry with its text cut and made safe (it came from a file).
    fn normalized(self) -> Self {
        Self {
            display: cut(
                &sanitize_display(&cut(&self.display, 4 * MAX_DISPLAY_BYTES)),
                MAX_DISPLAY_BYTES,
            ),
            ..self
        }
    }
}

/// One directory of flagged programs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub dir: String,
    pub display: String,
    /// Keyed by [`Program::key`]; at most [`MAX_DIR_PROGRAMS`].
    pub programs: BTreeMap<String, Program>,
    /// Connections of programs of this directory that did not fit the list.
    pub unlisted_connections: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

/// The summary: four classes, the drop ring and its counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Aggregate {
    /// When the first thing in it was seen (0: nothing yet); the file's age.
    pub created_ms: i64,
    pub a: BTreeMap<String, Program>,
    pub b: BTreeMap<String, DirEntry>,
    pub c: BTreeMap<String, Program>,
    pub d: BTreeMap<String, Program>,
    pub ring: VecDeque<Dropped>,
    pub refused: u64,
    pub evicted: u64,
    /// Some connections were missed (the feed lagged).
    pub incomplete: bool,
    /// Upper estimate of the serialised size of `a`, `b`, `c` and `d`. Kept
    /// up to date by every change; a value read from a file is never trusted
    /// (a merge rebuilds it).
    pub bytes: usize,
}

impl DirEntry {
    fn new(dir: &str, at_ms: i64) -> Self {
        let dir = cut(dir, MAX_PATH_BYTES);
        Self {
            display: cut(&sanitize_display(&dir), MAX_DISPLAY_BYTES),
            dir,
            first_seen_ms: at_ms,
            last_seen_ms: at_ms,
            ..Self::default()
        }
    }

    /// Whether a listed program has a network flag (it stayed here because
    /// class A was full).
    fn network_flagged(&self) -> bool {
        self.programs.values().any(Program::network_flagged)
    }

    fn connections(&self) -> u64 {
        self.programs
            .values()
            .map(|p| p.connections)
            .fold(self.unlisted_connections, u64::saturating_add)
    }

    /// What it is charged: the entry itself and its listed programs.
    fn bytes(&self) -> usize {
        MAX_DIR_BYTES
            + self
                .programs
                .values()
                .map(Program::est_bytes)
                .sum::<usize>()
    }

    fn see(&mut self, first_ms: i64, last_ms: i64) {
        self.first_seen_ms = if self.first_seen_ms == 0 {
            first_ms
        } else {
            self.first_seen_ms.min(first_ms)
        };
        self.last_seen_ms = self.last_seen_ms.max(last_ms);
    }
}

/// The directory a flagged program's entry is keyed by.
fn dir_of(program: &Program) -> String {
    program
        .raw_path
        .as_deref()
        .map(classify::parent_dir)
        .unwrap_or_default()
}

impl Aggregate {
    /// Hand one program's summary to the aggregator: it is merged into the
    /// program already recorded under the same key, put in the class its flags
    /// call for, and the class caps and the byte budget are kept.
    pub fn absorb(&mut self, program: Program) -> Absorbed {
        let program = program.normalized();
        self.created_ms = match (self.created_ms, program.first_seen_ms) {
            (0, first) => first,
            (created, 0) => created,
            (created, first) => created.min(first),
        };
        let protect_key = program.key().to_string();
        let protect_dir = dir_of(&program);
        // What this update can add, at most.
        let need = program.est_bytes() + MAX_DIR_BYTES;
        if self.bytes.saturating_add(need) > BYTE_BUDGET {
            self.make_room(need, &protect_key, &protect_dir);
        }
        let no_room = self.bytes.saturating_add(need) > BYTE_BUDGET;
        let (merged, old) = match self.take(&program) {
            // Counters only: no detail that would not fit.
            Some((existing, class)) if no_room => {
                (existing.merged_counts_only(program), Some(class))
            }
            Some((existing, class)) => (existing.merged(program), Some(class)),
            None if no_room => {
                self.refuse(&program);
                return Absorbed::Refused;
            }
            None => (program, None),
        };
        let mut class = merged.class();
        if class == Class::A && old != Some(Class::A) && self.a.len() >= CAP_A {
            match old {
                // Stays where it is, flagged, and is evicted last there.
                Some(stay) => class = stay,
                None => {
                    self.refuse(&merged);
                    return Absorbed::Refused;
                }
            }
        }
        self.place(class, merged);
        self.enforce_cap(class, &protect_key, &protect_dir);
        Absorbed::Recorded
    }

    /// `self` and `other` as one summary: counts summed, earliest first and
    /// latest last, the capped sets joined, and every cap applied again.
    pub fn merge(mut self, other: Aggregate) -> Aggregate {
        self.created_ms = match (self.created_ms, other.created_ms) {
            (0, created) | (created, 0) => created,
            (a, b) => a.min(b),
        };
        self.refused = self.refused.saturating_add(other.refused);
        self.evicted = self.evicted.saturating_add(other.evicted);
        self.incomplete |= other.incomplete;
        for dropped in other.ring {
            self.push_ring(dropped.normalized());
        }
        for program in other
            .a
            .into_values()
            .chain(other.c.into_values())
            .chain(other.d.into_values())
        {
            self.absorb(program);
        }
        for dir in other.b.into_values() {
            self.absorb_dir(dir);
        }
        self
    }

    /// Programs recorded now (the banner's number): a directory entry counts
    /// the programs it lists.
    pub fn programs_seen(&self) -> usize {
        self.a.len()
            + self.c.len()
            + self.d.len()
            + self.b.values().map(|dir| dir.programs.len()).sum::<usize>()
    }

    /// Programs refused or evicted so far.
    pub fn dropped(&self) -> u64 {
        self.refused.saturating_add(self.evicted)
    }

    /// Whether nothing was recorded or dropped.
    pub fn is_empty(&self) -> bool {
        self.a.is_empty()
            && self.b.is_empty()
            && self.c.is_empty()
            && self.d.is_empty()
            && self.dropped() == 0
    }

    /// The class the program with `key` ([`path_key`] or [`name_key`]) is in
    /// now, if it is recorded.
    pub fn class_of(&self, key: &str) -> Option<Class> {
        if self.a.contains_key(key) {
            Some(Class::A)
        } else if self.c.contains_key(key) {
            Some(Class::C)
        } else if self.d.contains_key(key) {
            Some(Class::D)
        } else if key
            .strip_prefix(PATH_KEY)
            .and_then(|path| self.b.get(&classify::parent_dir(path)))
            .is_some_and(|dir| dir.programs.contains_key(key))
        {
            Some(Class::B)
        } else {
            None
        }
    }

    /// The size estimate worked out from scratch (what `bytes` must equal).
    #[cfg(test)]
    pub(crate) fn recount(&self) -> usize {
        let programs =
            |m: &BTreeMap<String, Program>| m.values().map(Program::est_bytes).sum::<usize>();
        programs(&self.a)
            + programs(&self.c)
            + programs(&self.d)
            + self.b.values().map(DirEntry::bytes).sum::<usize>()
    }

    fn sub(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_sub(bytes);
    }

    fn add(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    /// Remove and return the program recorded under `program`'s key.
    fn take(&mut self, program: &Program) -> Option<(Program, Class)> {
        let key = program.key();
        let found = if let Some(found) = self.a.remove(key) {
            Some((found, Class::A))
        } else if let Some(found) = self.c.remove(key) {
            Some((found, Class::C))
        } else {
            self.d.remove(key).map(|found| (found, Class::D))
        };
        if let Some((found, class)) = found {
            self.sub(found.est_bytes());
            return Some((found, class));
        }
        let dir_key = dir_of(program);
        let dir = self.b.get_mut(&dir_key)?;
        let found = dir.programs.remove(key)?;
        let mut freed = found.est_bytes();
        if dir.programs.is_empty() && dir.unlisted_connections == 0 {
            self.b.remove(&dir_key);
            freed += MAX_DIR_BYTES;
        }
        self.sub(freed);
        Some((found, Class::B))
    }

    fn place(&mut self, class: Class, program: Program) {
        let key = program.key().to_string();
        let size = program.est_bytes();
        match class {
            Class::A => {
                self.a.insert(key, program);
            }
            Class::C => {
                self.c.insert(key, program);
            }
            Class::D => {
                self.d.insert(key, program);
            }
            Class::B => {
                let dir_key = dir_of(&program);
                let mut charge = 0;
                let dir = self.b.entry(dir_key.clone()).or_insert_with(|| {
                    charge = MAX_DIR_BYTES;
                    DirEntry::new(&dir_key, program.first_seen_ms)
                });
                dir.see(program.first_seen_ms, program.last_seen_ms);
                if dir.programs.len() < MAX_DIR_PROGRAMS || dir.programs.contains_key(&key) {
                    dir.programs.insert(key, program);
                    charge += size;
                } else {
                    dir.unlisted_connections =
                        dir.unlisted_connections.saturating_add(program.connections);
                }
                self.add(charge);
                return;
            }
        }
        self.add(size);
    }

    /// A directory entry from another summary: its programs one by one, then
    /// what it only counted.
    fn absorb_dir(&mut self, dir: DirEntry) {
        let dir_key = cut(&dir.dir, MAX_PATH_BYTES);
        for program in dir.programs.into_values() {
            self.absorb(program);
        }
        if dir.unlisted_connections == 0 {
            return;
        }
        let mut charge = 0;
        let entry = self.b.entry(dir_key.clone()).or_insert_with(|| {
            charge = MAX_DIR_BYTES;
            DirEntry::new(&dir_key, dir.first_seen_ms)
        });
        entry.unlisted_connections = entry
            .unlisted_connections
            .saturating_add(dir.unlisted_connections);
        entry.see(dir.first_seen_ms, dir.last_seen_ms);
        self.add(charge);
        self.enforce_cap(Class::B, "", &dir_key);
    }

    /// Evict within `class` until it is within its cap. The entry just
    /// touched (`protect_key`, or for B its directory) is never the victim,
    /// so a newcomer is not the first thing to go.
    fn enforce_cap(&mut self, class: Class, protect_key: &str, protect_dir: &str) {
        let cap = match class {
            Class::A => return,
            Class::B => CAP_B,
            Class::C => CAP_C,
            Class::D => CAP_D,
        };
        while self.class_len(class) > cap && self.evict_one(class, protect_key, protect_dir) {}
    }

    fn class_len(&self, class: Class) -> usize {
        match class {
            Class::A => self.a.len(),
            Class::B => self.b.len(),
            Class::C => self.c.len(),
            Class::D => self.d.len(),
        }
    }

    /// Free at least `need` bytes of the budget, if the entries that may be
    /// evicted (never class A) allow: directories, then names, then paths.
    fn make_room(&mut self, need: usize, protect_key: &str, protect_dir: &str) {
        while self.bytes.saturating_add(need) > BYTE_BUDGET {
            let evicted = [Class::B, Class::D, Class::C]
                .into_iter()
                .any(|class| self.evict_one(class, protect_key, protect_dir));
            if !evicted {
                break;
            }
        }
    }

    /// Evict the entry of `class` that goes first (see the module docs), never
    /// the protected one. False when there is none.
    fn evict_one(&mut self, class: Class, protect_key: &str, protect_dir: &str) -> bool {
        if class == Class::B {
            let victim = self
                .b
                .values()
                .filter(|d| d.dir != protect_dir)
                .min_by(|x, y| {
                    (x.network_flagged(), x.last_seen_ms, &x.dir).cmp(&(
                        y.network_flagged(),
                        y.last_seen_ms,
                        &y.dir,
                    ))
                })
                .map(|d| d.dir.clone());
            let Some(dir) = victim.and_then(|k| self.b.remove(&k)) else {
                return false;
            };
            self.sub(dir.bytes());
            self.evicted = self
                .evicted
                .saturating_add(dir.programs.len().max(1) as u64);
            self.push_ring(Dropped {
                display: dir.display.clone(),
                kind: DropKind::Evicted,
                class: Class::B,
                connections: dir.connections(),
                flagged: dir.network_flagged(),
            });
            return true;
        }
        let map = match class {
            Class::C => &mut self.c,
            Class::D => &mut self.d,
            _ => return false,
        };
        let order = |p: &Program| {
            let activity = if class == Class::C { p.connections } else { 0 };
            (p.network_flagged(), activity, p.last_seen_ms)
        };
        let victim = map
            .iter()
            .filter(|(key, _)| key.as_str() != protect_key)
            .min_by(|(kx, x), (ky, y)| (order(x), kx).cmp(&(order(y), ky)))
            .map(|(key, _)| key.clone());
        let Some(program) = victim.and_then(|k| map.remove(&k)) else {
            return false;
        };
        self.sub(program.est_bytes());
        self.evicted = self.evicted.saturating_add(1);
        self.push_ring(Dropped {
            display: program.display.clone(),
            kind: DropKind::Evicted,
            class,
            connections: program.connections,
            flagged: program.network_flagged(),
        });
        true
    }

    /// A new candidate that class A (or the byte budget) had no room for.
    fn refuse(&mut self, program: &Program) {
        let known = self
            .ring
            .iter()
            .any(|d| d.kind == DropKind::Refused && d.display == program.display);
        if !known {
            self.refused = self.refused.saturating_add(1);
        }
        self.push_ring(Dropped {
            display: program.display.clone(),
            kind: DropKind::Refused,
            class: program.class(),
            connections: program.connections,
            flagged: program.network_flagged(),
        });
    }

    fn push_ring(&mut self, dropped: Dropped) {
        push_ring(&mut self.ring, dropped);
    }
}

/// Add to the ring: the same key and kind again only adds its connections (and
/// moves it to the newest end); the oldest entries fall off past [`RING_LEN`].
fn push_ring(ring: &mut VecDeque<Dropped>, dropped: Dropped) {
    let entry = match ring
        .iter()
        .position(|d| d.kind == dropped.kind && d.display == dropped.display)
        .and_then(|at| ring.remove(at))
    {
        Some(mut same) => {
            same.connections = same.connections.saturating_add(dropped.connections);
            same.flagged |= dropped.flagged;
            same
        }
        None => dropped,
    };
    ring.push_back(entry);
    while ring.len() > RING_LEN {
        ring.pop_front();
    }
}

#[cfg(test)]
#[path = "aggregate_tests.rs"]
mod tests;
