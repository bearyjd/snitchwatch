//! S1: the one field-cap table. Every string the history store persists has a
//! fixed maximum byte length, so the largest possible row, index entry and
//! snapshot are known from the schema (the S2 page arithmetic builds on it).
//!
//! Two layers enforce it, so neither is the only guard: the insert API takes
//! only the capped types below (a value is cut to its cap, on a UTF-8
//! boundary, before it is bound), and `schema.rs` repeats every cap as a
//! `CHECK (length(CAST(col AS BLOB)) <= cap)`, so even a statement that
//! bypasses the API cannot put a longer value in the file.

use std::borrow::Cow;

/// Set by the owner (design decision 2).
pub const RULE_NAME: usize = 256;
pub const PROCESS_PATH: usize = 4096;
pub const DST_HOST: usize = 253;

/// Derived from the schema and the sources of the values; **proposed**, not
/// owner-set. See the PR description for the reasoning behind each.
pub const PROTOCOL: usize = 16;
pub const RULE_CLASS: usize = 24;
/// The longest text `IpAddr::to_string` produces (what is stored; no zone
/// id): eight full groups, `ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff`. Rust
/// prints a mapped address as `::ffff:255.255.255.255` (22 bytes) and an
/// IPv4 address in at most 15.
pub const IP_TEXT: usize = 39;
/// The design's "8 KiB per snapshot" (D7 Bounds).
pub const SNAPSHOT: usize = 8 * 1024;
/// SHA-256 truncated to 128 bits (D7 identity).
pub const RULE_HASH: usize = 16;

/// What replaces the cut-off tail of a truncated value. It counts inside the
/// cap, so a truncated value is exactly as long as the cap allows.
pub const MARKER: &str = "\u{2026}";

/// `value` unchanged when it fits in `cap` bytes; otherwise its longest
/// prefix that leaves room for [`MARKER`], on a `char` boundary, plus the
/// marker. The result is never longer than `cap`.
pub fn bound(value: &str, cap: usize) -> Cow<'_, str> {
    if value.len() <= cap {
        return Cow::Borrowed(value);
    }
    // A cap with no room for the marker (none of ours) is a plain cut.
    let room = cap.checked_sub(MARKER.len());
    let mut end = room.unwrap_or(cap);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    match room {
        Some(_) => Cow::Owned(format!("{}{MARKER}", &value[..end])),
        None => Cow::Borrowed(&value[..end]),
    }
}

/// A string cut to `CAP` bytes on construction. The only way to hand a text
/// column to the insert API.
#[derive(Clone, PartialEq, Eq)]
pub struct Capped<const CAP: usize>(String);

impl<const CAP: usize> Capped<CAP> {
    pub fn new(value: &str) -> Self {
        Self(bound(value, CAP).into_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Bytes the string has allocated (its capacity, not its length).
    pub fn allocated(&self) -> usize {
        self.0.capacity()
    }
}

impl<const CAP: usize> std::fmt::Debug for Capped<CAP> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Paths and hosts are user-chosen text: keep them out of logs.
        write!(f, "Capped<{CAP}>({} bytes)", self.0.len())
    }
}

pub type RuleName = Capped<RULE_NAME>;
pub type ProcessPath = Capped<PROCESS_PATH>;
pub type DstHost = Capped<DST_HOST>;
pub type Protocol = Capped<PROTOCOL>;
pub type RuleClass = Capped<RULE_CLASS>;

/// A redacted rule snapshot (D7). JSON, so it is **refused** when too long
/// rather than cut into invalid JSON; PR 4 builds one that fits.
#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot(String);

impl Snapshot {
    pub fn new(value: &str) -> Option<Self> {
        (value.len() <= SNAPSHOT).then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn allocated(&self) -> usize {
        self.0.capacity()
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Snapshot({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_within_the_cap_is_unchanged() {
        assert_eq!(bound("abc", 3), "abc");
        assert!(matches!(bound("abc", 3), Cow::Borrowed(_)));
    }

    #[test]
    fn a_value_over_the_cap_ends_in_the_marker_inside_the_cap() {
        let cut = bound("abcdefghij", 8);
        assert_eq!(cut, format!("abcde{MARKER}"));
        assert_eq!(cut.len(), 8);
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // Each char is 3 bytes; a cap of 10 leaves 7 for text: two chars (6).
        let cut = bound("\u{4e2d}\u{4e2d}\u{4e2d}\u{4e2d}\u{4e2d}", 10);
        assert!(cut.len() <= 10);
        assert!(cut.ends_with(MARKER));
        assert_eq!(cut.chars().count(), 3);
    }

    #[test]
    fn a_cap_smaller_than_the_marker_still_holds() {
        assert_eq!(bound("abcdef", 2), "ab");
    }

    #[test]
    fn snapshots_over_the_cap_are_refused_not_cut() {
        assert!(Snapshot::new(&"a".repeat(SNAPSHOT)).is_some());
        assert!(Snapshot::new(&"a".repeat(SNAPSHOT + 1)).is_none());
    }
}
