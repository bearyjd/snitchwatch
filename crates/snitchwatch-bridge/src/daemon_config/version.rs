//! The daemon's version, as far as the bridge can read it: the string in
//! `Statistics.daemon_version` (`ping`; `core.Version` in
//! `vendor/opensnitch/daemon/statistics/stats.go`). `ClientConfig.version`
//! (`subscribe`) is not it: the daemon fills that with the host's kernel
//! release (`core.GetKernelVersion()`, `daemon/ui/notifications.go`), so a
//! 1.8 daemon on kernel 6.x would read as 6.x. The string is not semver in
//! practice (`"v1.9.0-rc1"`), so only the first `major.minor` in it counts.
//! What the bridge refuses because of it is
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

    /// The first `<digits>.<digits>` anywhere in `raw` whose numbers both
    /// fit a `u32`, or `None` when there is none. A pair that doesn't fit is
    /// skipped whole (its minor is not reused as the next major), so
    /// `"99999999999.1 1.9.0"` reads as `1.9`.
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
                let major = raw[start..major_end].parse::<u32>().ok();
                let minor = raw[minor_start..minor_end].parse::<u32>().ok();
                if let (Some(major), Some(minor)) = (major, minor) {
                    return Some(Self::new(major, minor));
                }
                start = minor_end;
                continue;
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

/// The version from the daemon's latest `ping` that carried a readable
/// `Statistics.daemon_version`, shared between the gRPC service (which
/// writes it) and every path that checks a rule before sending it.
///
/// `None` until the first such `ping`, which means v1.8.0's rules: a 1.9
/// daemon could be sent a rule it won't compile in that window, and its own
/// `CHANGE_RULE` ERROR is the backstop. A `subscribe` never writes it
/// (`ClientConfig.version` is the kernel release), so a second or unrelated
/// `subscribe` can't weaken a known 1.9. A daemon that restarts as another
/// version keeps the old value until its next `ping`, which overwrites it;
/// an unreadable `ping` value leaves it alone.
#[derive(Debug, Clone, Default)]
pub struct SharedDaemonVersion(Arc<Mutex<Option<DaemonVersion>>>);

impl SharedDaemonVersion {
    /// Replace the version, readable or not.
    pub fn set(&self, version: Option<DaemonVersion>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = version;
    }

    /// What a `ping` reported: replaces the version when readable, leaves
    /// it alone when not.
    pub fn observe(&self, version: Option<DaemonVersion>) {
        if version.is_some() {
            self.set(version);
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
            ("99999999999.1 1.9.0", Some((1, 9))),
            ("1.99999999999 2.3", Some((2, 3))),
            ("99999999999.1.9", None),
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
    fn a_ping_overwrites_when_readable_and_is_ignored_when_not() {
        let shared = SharedDaemonVersion::default();
        assert_eq!(shared.get(), None);
        shared.observe(DaemonVersion::parse("garbage"));
        assert_eq!(shared.get(), None);
        shared.observe(DaemonVersion::parse("1.9.0"));
        assert_eq!(shared.get(), Some(DaemonVersion::new(1, 9)));
        shared.observe(DaemonVersion::parse(""));
        assert_eq!(shared.clone().get(), Some(DaemonVersion::new(1, 9)));
        shared.observe(DaemonVersion::parse("1.8.0"));
        assert_eq!(shared.get(), Some(DaemonVersion::new(1, 8)));
        shared.set(None);
        assert_eq!(shared.get(), None);
    }
}
