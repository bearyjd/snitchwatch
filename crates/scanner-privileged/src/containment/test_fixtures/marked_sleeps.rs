use super::me;
use super::signal as fixture_signal;
use crate::containment::{live_descendants, read_stat, vanished};
use std::collections::HashSet;
use std::io;
use std::time::{Duration, Instant};

/// The live `sleep <marker>` processes below `root` only: a process
/// elsewhere with the same argv is never counted. On drop the sleepers it
/// ever saw are killed by pid first (no /proc scan needed), then
/// whatever a best-effort scan still finds below `root`; killing `root`
/// itself is left to its owner, which must be declared before this.
pub(crate) struct MarkedSleeps {
    marker: String,
    root: i32,
    /// Marked sleepers seen so far.
    known: std::sync::Mutex<Vec<i32>>,
    scan: fn(i32, Instant) -> io::Result<Option<Vec<i32>>>,
    /// A file the fixture script appends the pids it starts to (`$$` of
    /// an escaped shell first, then each `$!`): known without any scan.
    pid_file: Option<std::path::PathBuf>,
}
impl MarkedSleeps {
    pub(crate) fn new(marker: String, root: i32) -> Self {
        Self::with_scan(marker, root, live_descendants)
    }

    pub(crate) fn with_scan(
        marker: String,
        root: i32,
        scan: fn(i32, Instant) -> io::Result<Option<Vec<i32>>>,
    ) -> Self {
        assert!(fixture_signal::target(root).is_some(), "unsafe root {root}");
        assert_ne!(root, me(), "the test process is never a fixture root");
        Self {
            marker,
            root,
            known: std::sync::Mutex::new(Vec::new()),
            scan,
            pid_file: None,
        }
    }

    pub(crate) fn with_pid_file(mut self, file: std::path::PathBuf) -> Self {
        self.pid_file = Some(file);
        self
    }

    fn want(&self) -> Vec<u8> {
        format!("sleep\0{}\0", self.marker).into_bytes()
    }

    pub(crate) fn record(&self, pids: &[i32]) {
        if let Ok(mut known) = self.known.lock() {
            known.extend_from_slice(pids);
        }
    }

    /// One observation. A truncated or failed scan, or a cmdline that
    /// cannot be read for any reason but the process having vanished, is
    /// an error: it says nothing about survivors.
    pub(crate) fn try_alive(&self, deadline: Instant) -> io::Result<Vec<i32>> {
        let want = self.want();
        let below = (self.scan)(self.root, deadline)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "scan hit its deadline"))?;
        let mut found = Vec::new();
        for pid in below {
            match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(cmdline) if cmdline == want => found.push(pid),
                Ok(_) => {}
                Err(e) if vanished(&e) => {}
                Err(e) => return Err(e),
            }
        }
        self.record(&found);
        Ok(found)
    }

    /// [`Self::try_alive`], retried a bounded number of times; then the
    /// test fails loudly rather than reading "none".
    pub(crate) fn alive(&self) -> Vec<i32> {
        let mut last = None;
        for _ in 0..20 {
            match self.try_alive(Instant::now() + Duration::from_secs(5)) {
                Ok(found) => return found,
                Err(e) => last = Some(e),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("could not observe the fixture's survivors: {last:?}");
    }
}
/// Whether a chain of live parents leads from `pid` up to `root`.
/// A true result requires a live process whose parent is `root`, and
/// `root` is always an unreaped own child (`own_root()`/`Reaped`, asserted
/// to be above 1 and not `me()`), so nothing outside the fixture can
/// become its child. Every other hop is pinned: `pid` is held by the
/// caller's pidfd; its parent is read, pinned with a pidfd, and `pid`'s
/// parent is read again, which must be unchanged (else the number was
/// recycled between the read and the pin); the same for each ancestor,
/// all pidfds kept until the walk ends. Any error or surprise spares.
pub(crate) fn descends_from(pid: i32, root: i32) -> bool {
    descends_from_with(pid, root, fixture_signal::open_pidfd, read_stat)
}

pub(crate) fn descends_from_with(
    pid: i32,
    root: i32,
    open: impl Fn(i32) -> io::Result<std::os::fd::OwnedFd>,
    stat: impl Fn(i32) -> io::Result<Option<(i32, bool)>>,
) -> bool {
    let mut held = Vec::new();
    let mut current = pid;
    for _ in 0..64 {
        let Ok(Some((parent, _))) = stat(current) else {
            return false;
        };
        if parent == root {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        match open(parent) {
            Ok(fd) => held.push(fd),
            Err(_) => return false,
        }
        // Pinned: `current`'s parent must still be it, else the number
        // was recycled between the read and the pin.
        if !matches!(stat(current), Ok(Some((again, _))) if again == parent) {
            return false;
        }
        current = parent;
    }
    false
}

impl MarkedSleeps {
    /// The cleanup `drop` runs (idempotent), with what each pid-file or
    /// recorded kill came to. Never panics, whatever /proc says. Every
    /// signal goes through a pidfd whose process is verified while held.
    pub(crate) fn cleanup(&self) -> Vec<(i32, fixture_signal::Verified)> {
        let mut results = Vec::new();
        // First what the fixture itself reported (no /proc scan needed),
        // in order: an escaped shell is listed before its sleepers, so it
        // stops forking before they are killed. Then what a scan saw.
        let reported = self
            .pid_file
            .as_ref()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|text| fixture_signal::complete_pids(&text))
            .unwrap_or_default();
        let known = self.known.lock().map(|k| k.clone()).unwrap_or_default();
        let mut seen = HashSet::new();
        for pid in reported.into_iter().chain(known) {
            if !seen.insert(pid) {
                continue;
            }
            results.push((
                pid,
                fixture_signal::kill_marked_patiently(pid, &self.marker),
            ));
        }
        // Then what a best-effort scan (errors ignored) still finds below
        // `root`: its own subtree only, a few passes for late forks.
        for _ in 0..3 {
            let below = (self.scan)(self.root, Instant::now() + Duration::from_secs(5))
                .ok()
                .flatten()
                .unwrap_or_default();
            if below.is_empty() {
                break;
            }
            // The scan is stale by now: a pid is signalled only if, with
            // its pidfd held, it carries the marker or a live chain of
            // parents leads from it to the reserved `root`.
            for pid in below.into_iter().rev() {
                fixture_signal::kill_verified(pid, |p| {
                    fixture_signal::has_marker(p, &self.marker) || descends_from(p, self.root)
                });
            }
        }
        results
    }
}
impl Drop for MarkedSleeps {
    fn drop(&mut self) {
        self.cleanup();
    }
}
