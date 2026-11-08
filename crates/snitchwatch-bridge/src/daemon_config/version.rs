//! The daemon's version, as far as the bridge can read it: the string in
//! `ClientConfig.version` (`subscribe`) or, when that gives none,
//! `Statistics.daemon_version` (`ping`). Neither is semver in practice
//! (`"mock-1.8.0"`, `"v1.9.0-rc1"`), so only the first `major.minor` in the
//! string counts. What the bridge refuses because of it is
//! `crate::rule_policy::PolicyContext`.

use std::sync::{Arc, Mutex};

/// A daemon's `major.minor`. Ordered by major, then minor, so `1.10` is
/// newer than `1.9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DaemonVersion {
    pub major: u32,
    pub minor: u32,
}

impl DaemonVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// The first `<digits>.<digits>` anywhere in `raw`, or `None` when
    /// there is none or a number doesn't fit a `u32`.
    pub fn parse(raw: &str) -> Option<Self> {
        let bytes = raw.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            if !bytes[start].is_ascii_digit() {
                start += 1;
                continue;
            }
            let major_end = digits_end(bytes, start);
            let minor_start = major_end + 1;
            let dotted = bytes.get(major_end) == Some(&b'.')
                && bytes.get(minor_start).is_some_and(u8::is_ascii_digit);
            if dotted {
                // Digits and `.` are single bytes: these are char boundaries.
                let minor_end = digits_end(bytes, minor_start);
                let major = raw[start..major_end].parse().ok()?;
                let minor = raw[minor_start..minor_end].parse().ok()?;
                return Some(Self::new(major, minor));
            }
            start = major_end;
        }
        None
    }
}

/// Where the run of ASCII digits starting at `start` ends.
fn digits_end(bytes: &[u8], start: usize) -> usize {
    start
        + bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
}

/// The version from the daemon's latest `subscribe`, shared between the
/// gRPC service (which writes it) and every path that checks a rule before
/// sending it. `None` until a daemon reports one the bridge can read. Last
/// write wins: a daemon that restarts as another version corrects it at its
/// next `subscribe`.
#[derive(Debug, Clone, Default)]
pub struct SharedDaemonVersion(Arc<Mutex<Option<DaemonVersion>>>);

impl SharedDaemonVersion {
    /// What a `subscribe` reported, readable or not.
    pub fn set(&self, version: Option<DaemonVersion>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = version;
    }

    /// What a `ping` reported, kept only while no version is known.
    pub fn set_if_unknown(&self, version: Option<DaemonVersion>) {
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if current.is_none() {
            *current = version;
        }
    }

    pub fn get(&self) -> Option<DaemonVersion> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_major_minor_anywhere_is_the_version() {
        let cases: &[(&str, Option<(u32, u32)>)] = &[
            ("1.8.0", Some((1, 8))),
            ("mock-1.8.0", Some((1, 8))),
            ("mock-1.6.0", Some((1, 6))),
            ("v1.9.0-rc1", Some((1, 9))),
            ("1.9", Some((1, 9))),
            ("1.10", Some((1, 10))),
            ("opensnitch 2.0.1 (fork 3.4)", Some((2, 0))),
            ("x1y2.3", Some((2, 3))),
            ("", None),
            ("garbage", None),
            ("1", None),
            ("1.", None),
            (".9", None),
            ("v1-9", None),
            ("99999999999.1", None),
        ];
        for (raw, want) in cases {
            let want = want.map(|(major, minor)| DaemonVersion::new(major, minor));
            assert_eq!(DaemonVersion::parse(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(DaemonVersion::new(1, 10) > DaemonVersion::new(1, 9));
        assert!(DaemonVersion::new(2, 0) > DaemonVersion::new(1, 99));
        assert!(DaemonVersion::new(1, 8) < DaemonVersion::new(1, 9));
    }

    #[test]
    fn subscribe_overwrites_and_ping_only_fills_a_gap() {
        let shared = SharedDaemonVersion::default();
        assert_eq!(shared.get(), None);
        shared.set_if_unknown(DaemonVersion::parse("mock-1.8.0"));
        assert_eq!(shared.get(), Some(DaemonVersion::new(1, 8)));
        shared.set_if_unknown(DaemonVersion::parse("1.9.0"));
        assert_eq!(shared.clone().get(), Some(DaemonVersion::new(1, 8)));
        shared.set(DaemonVersion::parse("1.9.0"));
        assert_eq!(shared.get(), Some(DaemonVersion::new(1, 9)));
        shared.set(DaemonVersion::parse("garbage"));
        assert_eq!(shared.get(), None);
    }
}
