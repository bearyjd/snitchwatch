//! One program's summary (phase L2): see `aggregate` for how programs are
//! sorted into classes and bounded.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::aggregate::Class;
use super::classify;
use crate::app_names::sanitize_display;
use crate::bridge_runtime::PendingRow;

pub const MAX_DESTINATIONS: usize = 64;
pub const MAX_PORTS: usize = 32;
pub const MAX_PROTOCOLS: usize = 4;
/// Longest path kept; a longer one is cut and the program marked `truncated`
/// (so no exact-path rule is ever built from it).
pub(super) const MAX_PATH_BYTES: usize = 1024;
/// Longest raw process name kept.
pub const MAX_NAME_BYTES: usize = 256;
/// Longest display copy kept.
pub const MAX_DISPLAY_BYTES: usize = 256;
/// Longest destination kept (a DNS name is at most 253 bytes).
pub const MAX_DEST_BYTES: usize = 253;
/// Longest protocol word kept.
const MAX_PROTOCOL_BYTES: usize = 16;

/// Prefix of a [`Program::key`] that is an executable path.
pub const PATH_KEY: &str = "path:";
/// Prefix of a [`Program::key`] that is a raw process name, so a process named
/// `/tmp/x` and the program at `/tmp/x` never share an entry.
pub const NAME_KEY: &str = "name:";

/// What a program is recorded under.
pub fn path_key(path: &str) -> String {
    format!("{PATH_KEY}{path}")
}

/// What a nameless-path program is recorded under.
pub fn name_key(name: &str) -> String {
    format!("{NAME_KEY}{name}")
}

/// The most JSON bytes a text of `len` bytes can take: every byte a quote or
/// backslash (two bytes), plus its quotes and separator. Control characters,
/// which JSON would write as six bytes, are replaced before anything is
/// stored, so two is the worst case.
const fn text_bytes(len: usize) -> usize {
    len * 2 + 4
}

/// Fixed part of a serialised program: field names, numbers and punctuation.
const PROGRAM_FIXED_BYTES: usize = 512;

/// The most serialised bytes one program can take under the per-field limits
/// (name, path, display, key twice (field and map key), 64 destinations, 32
/// ports, 4 protocols). `Program::est_bytes` never exceeds it.
pub const MAX_PROGRAM_BYTES: usize = PROGRAM_FIXED_BYTES
    + text_bytes(MAX_NAME_BYTES)
    + text_bytes(MAX_PATH_BYTES)
    + text_bytes(MAX_DISPLAY_BYTES)
    + 2 * text_bytes(MAX_PATH_BYTES + PATH_KEY.len())
    + MAX_DESTINATIONS * (text_bytes(MAX_DEST_BYTES) + 24)
    + MAX_PORTS * 8
    + MAX_PROTOCOLS * (text_bytes(MAX_PROTOCOL_BYTES) + 2);

/// Everything seen of one program.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// The raw process name (never shown).
    pub raw_name: String,
    /// The raw executable path (never shown; the exact key a rule needs).
    pub raw_path: Option<String>,
    /// What it is recorded under: `path:<raw_path>`, else `name:<raw_name>`.
    #[serde(default)]
    pub key: String,
    /// The path, else the name, made safe to show.
    pub display: String,
    /// The raw path was cut or had control characters replaced, so it is not
    /// the program's exact path: no exact-path rule may be built from it.
    #[serde(default)]
    pub truncated: bool,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub connections: u64,
    pub bare_ip: u64,
    pub uncommon_port: u64,
    /// Destination -> connections, for the first [`MAX_DESTINATIONS`] seen.
    pub destinations: BTreeMap<String, u64>,
    /// Connections to destinations beyond those kept (so "64 and N more": the
    /// distinct count past 64 cannot be known without storing them).
    pub dest_overflow: u64,
    pub ports: BTreeSet<u16>,
    pub protocols: BTreeSet<String>,
}

pub(super) fn cut(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// `text` with control characters replaced by `replacement`, cut to
/// `max_bytes`; and whether that changed anything.
fn tidy(text: &str, max_bytes: usize, replacement: char) -> (String, bool) {
    let replaced: String = text
        .chars()
        .map(|c| if c.is_control() { replacement } else { c })
        .collect();
    let tidied = cut(&replaced, max_bytes);
    let changed = tidied != text;
    (tidied, changed)
}

impl Program {
    /// One connection, seen at `now_ms`, as a summary of one connection.
    pub fn observed(row: &PendingRow, now_ms: i64) -> Self {
        let destination = classify::destination(&row.dst_host, &row.dst_ip);
        let bare = classify::is_bare_ip(&destination);
        let uncommon = classify::is_uncommon_port(row.port, &row.protocol);
        Self {
            raw_name: row.process.clone(),
            raw_path: row.process_path.clone().filter(|p| !p.trim().is_empty()),
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
            connections: 1,
            bare_ip: u64::from(bare),
            uncommon_port: u64::from(uncommon),
            destinations: BTreeMap::from([(destination, 1)]),
            ports: BTreeSet::from([row.port]),
            protocols: BTreeSet::from([row.protocol.trim().to_lowercase()]),
            ..Self::default()
        }
        .normalized()
    }

    /// The identity (`path:<path>`, else `name:<name>`), set by
    /// [`normalized`](Self::normalized).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The most serialised bytes this program takes (an upper bound).
    pub fn est_bytes(&self) -> usize {
        PROGRAM_FIXED_BYTES
            + text_bytes(self.raw_name.len())
            + self.raw_path.as_deref().map_or(0, |p| text_bytes(p.len()))
            + text_bytes(self.display.len())
            + 2 * text_bytes(self.key.len())
            + self
                .destinations
                .keys()
                .map(|d| text_bytes(d.len()) + 24)
                .sum::<usize>()
            + self.ports.len() * 8
            + self
                .protocols
                .iter()
                .map(|p| text_bytes(p.len()) + 2)
                .sum::<usize>()
    }

    /// Whether any connection went to a bare IP or an uncommon port.
    pub fn network_flagged(&self) -> bool {
        self.bare_ip > 0 || self.uncommon_port > 0
    }

    /// Whether the program runs from a flagged place.
    pub fn path_flagged(&self) -> bool {
        self.raw_path
            .as_deref()
            .is_some_and(classify::is_path_flagged)
    }

    /// The class this program belongs in, caps aside.
    pub fn class(&self) -> Class {
        if self.network_flagged() {
            Class::A
        } else if self.path_flagged() {
            Class::B
        } else if self.raw_path.is_some() {
            Class::C
        } else {
            Class::D
        }
    }

    /// The same data with every text cut and made safe and every set capped,
    /// so a summary read from a file cannot grow a record past its limits.
    pub fn normalized(mut self) -> Self {
        self.raw_name = tidy(&self.raw_name, MAX_NAME_BYTES, '?').0;
        if let Some(path) = self.raw_path.take() {
            let (path, changed) = tidy(&path, MAX_PATH_BYTES, '?');
            self.truncated |= changed;
            self.raw_path = Some(path);
        }
        self.key = match &self.raw_path {
            Some(path) => path_key(path),
            None => name_key(&self.raw_name),
        };
        let shown = self.raw_path.as_deref().unwrap_or(&self.raw_name);
        self.display = cut(&sanitize_display(shown), MAX_DISPLAY_BYTES);
        let mut cut_destinations = BTreeMap::new();
        for (dest, count) in std::mem::take(&mut self.destinations) {
            let slot: &mut u64 = cut_destinations
                .entry(tidy(&dest, MAX_DEST_BYTES, '_').0)
                .or_default();
            *slot = slot.saturating_add(count);
        }
        self.destinations = cut_destinations;
        self.protocols = std::mem::take(&mut self.protocols)
            .into_iter()
            .map(|p| tidy(&p, MAX_PROTOCOL_BYTES, '_').0)
            .collect();
        self.cap_sets();
        self
    }

    /// Keep the destinations with the most connections (then by name), and the
    /// lowest ports; the connections of a dropped destination are counted.
    fn cap_sets(&mut self) {
        if self.destinations.len() > MAX_DESTINATIONS {
            let mut all: Vec<(String, u64)> =
                std::mem::take(&mut self.destinations).into_iter().collect();
            all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (_, count) in all.split_off(MAX_DESTINATIONS) {
                self.dest_overflow = self.dest_overflow.saturating_add(count);
            }
            self.destinations = all.into_iter().collect();
        }
        while self.ports.len() > MAX_PORTS {
            self.ports.pop_last();
        }
        while self.protocols.len() > MAX_PROTOCOLS {
            self.protocols.pop_last();
        }
    }

    /// `self` and `other` (the same program) as one: counts summed, earliest
    /// first, latest last, the capped sets joined and capped again.
    pub fn merged(mut self, other: Program) -> Self {
        self.add_counts(&other);
        for (dest, count) in other.destinations {
            let slot = self.destinations.entry(dest).or_insert(0);
            *slot = slot.saturating_add(count);
        }
        self.ports.extend(other.ports);
        self.protocols.extend(other.protocols);
        self.cap_sets();
        self
    }

    /// Like [`merged`](Self::merged), but nothing new is kept: destinations
    /// not already listed are only counted, and no port or protocol is added.
    /// For when the byte budget has no room for more detail.
    pub fn merged_counts_only(mut self, other: Program) -> Self {
        self.add_counts(&other);
        for (dest, count) in other.destinations {
            match self.destinations.get_mut(&dest) {
                Some(slot) => *slot = slot.saturating_add(count),
                None => self.dest_overflow = self.dest_overflow.saturating_add(count),
            }
        }
        self
    }

    fn add_counts(&mut self, other: &Program) {
        self.first_seen_ms = self.first_seen_ms.min(other.first_seen_ms);
        self.last_seen_ms = self.last_seen_ms.max(other.last_seen_ms);
        self.connections = self.connections.saturating_add(other.connections);
        self.bare_ip = self.bare_ip.saturating_add(other.bare_ip);
        self.uncommon_port = self.uncommon_port.saturating_add(other.uncommon_port);
        self.dest_overflow = self.dest_overflow.saturating_add(other.dest_overflow);
        self.truncated |= other.truncated;
    }
}
