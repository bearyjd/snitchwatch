//! A scanner re-exec test's whole cleanup in one `Drop`: the scanner child
//! (its own process group), a unique marker, and a pid file the fixture
//! reports pids into. Nothing a test must call before its first `assert!`.

use super::marked_sleeps::MarkedSleeps;
use super::signal as fixture_signal;
use fixture_signal::Verified;
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Env var carrying a fixture's unique marker to the re-exec'd scanner.
pub(crate) const MARKER_ENV: &str = "SNITCHWATCH_TEST_MARKER";

pub(crate) struct ScannerFixture {
    /// `None` once reaped by [`Self::finish`] or the drop.
    child: Option<Child>,
    marker: String,
    pid_file: PathBuf,
    /// Declared last: removed after `drop` has used the pid file.
    _dir: tempfile::TempDir,
}

impl ScannerFixture {
    /// Spawns `program` as the leader of its own process group, stdio null.
    /// `configure` adds args and env, given the marker and the pid file path.
    pub(crate) fn spawn(
        program: impl AsRef<OsStr>,
        configure: impl FnOnce(&mut Command, &str, &Path),
    ) -> Self {
        let dir = tempfile::tempdir().expect("fixture tempdir");
        let pid_file = dir.path().join("pids");
        let marker = fixture_signal::unique_marker();
        let mut command = Command::new(program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        configure(&mut command, &marker, &pid_file);
        let child = command.spawn().expect("spawn the scanner fixture");
        Self {
            child: Some(child),
            marker,
            pid_file,
            _dir: dir,
        }
    }

    /// Re-executes this test binary at the single test `test`, with the
    /// marker as env and as an extra argv (it matches no test name).
    pub(crate) fn reexec(test: &str, configure: impl FnOnce(&mut Command, &Path)) -> Self {
        let exe = std::env::current_exe().expect("current_exe");
        Self::spawn(exe, |command, marker, file| {
            command
                .args(["--exact", test, "--test-threads=1", marker])
                .env(MARKER_ENV, marker);
            configure(command, file);
        })
    }

    pub(crate) fn marker(&self) -> &str {
        &self.marker
    }

    /// The pids the fixture reported (newline-terminated lines only).
    pub(crate) fn reported(&self) -> Vec<i32> {
        std::fs::read_to_string(&self.pid_file)
            .map(|text| fixture_signal::complete_pids(&text))
            .unwrap_or_default()
    }

    /// Reports `pid` for cleanup (killed on drop if it still carries the marker).
    pub(crate) fn record_pid(&self, pid: i32) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.pid_file)
            .expect("open the pid file");
        file.write_all(format!("{pid}\n").as_bytes())
            .expect("record a pid");
    }

    /// Whether the scanner exited within `bound`, observed without reaping.
    pub(crate) fn exited_within(&self, bound: Duration) -> bool {
        exited_within(self.child.as_ref().expect("already finished"), bound)
    }

    /// The full cleanup now: the scanner's status and the cleanup problems
    /// (refused signals). Assert the problems empty AFTER the test's own
    /// assertions, so a cleanup problem never masks the real failure.
    pub(crate) fn finish(&mut self) -> (ExitStatus, Vec<String>) {
        let (status, problems) = self.teardown().expect("already finished");
        (status.expect("the scanner could not be reaped"), problems)
    }

    /// Recorded pids (pidfd-verified), a marker scan below the scanner, the
    /// verified group kill, then kill and reap the owned child. A refused
    /// signal never skips a step; it is returned. `None` if already done.
    fn teardown(&mut self) -> Option<(Option<ExitStatus>, Vec<String>)> {
        let mut child = self.child.take()?;
        let root = child.id() as i32;
        let mut problems = Vec::new();
        let sleeps =
            MarkedSleeps::new(self.marker.clone(), root).with_pid_file(self.pid_file.clone());
        for (pid, verdict) in sleeps.cleanup() {
            if matches!(verdict, Verified::Refused(_)) {
                problems.push(format!("pid {pid}: {verdict:?}"));
            }
        }
        let group = fixture_signal::killpg_marked(root, &self.marker);
        if !matches!(group, Verified::Sent | Verified::Gone) {
            problems.push(format!("group {root}: {group:?}"));
        }
        // The child is ours and unreaped: signalling it cannot hit a recycled pid.
        let killed = child.kill();
        let status = match &killed {
            Ok(()) => child.wait().ok(),
            Err(e) => {
                problems.push(format!("kill of scanner {root}: {e}"));
                None
            }
        };
        Some((status, problems))
    }
}

impl Drop for ScannerFixture {
    /// Cleans up if [`Self::finish`] never ran, and reports its problems
    /// (printed instead while unwinding, where a panic would abort).
    fn drop(&mut self) {
        let Some((_, problems)) = self.teardown() else {
            return;
        };
        if problems.is_empty() {
            return;
        }
        let report = format!("fixture cleanup refused: {}", problems.join("; "));
        if std::thread::panicking() {
            eprintln!("{report}");
        } else {
            panic!("{report}");
        }
    }
}

/// Whether `child` has exited, observed without reaping it, so its pid
/// (and process group) stays reserved and a later group kill can never
/// name a recycled, unrelated group. Polls until `bound` has passed.
pub(crate) fn exited_within(child: &Child, bound: Duration) -> bool {
    let start = Instant::now();
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let done = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == 0
            && unsafe { info.si_pid() } != 0;
        if done || start.elapsed() > bound {
            return done;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// The fixture's drop is the only cleanup: a test that panics before
    /// calling anything still loses no recorded pid.
    #[test]
    fn drop_kills_a_recorded_pid_when_the_test_panics_before_cleanup() {
        let sleeper = std::cell::RefCell::new(None);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fixture = ScannerFixture::spawn("/usr/bin/sleep", |command, marker, _| {
                command.arg(marker);
            });
            let throwaway = Command::new("/usr/bin/sleep")
                .arg(fixture.marker())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            fixture.record_pid(throwaway.id() as i32);
            *sleeper.borrow_mut() = Some(throwaway);
            panic!("the test fails before any cleanup");
        }));
        assert!(outcome.is_err());
        let mut sleeper: Child = sleeper.into_inner().expect("the sleeper was spawned");
        let start = Instant::now();
        let mut status = None;
        while status.is_none() && start.elapsed() < Duration::from_secs(5) {
            status = sleeper.try_wait().unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = sleeper.kill();
        let _ = sleeper.wait();
        assert_eq!(
            status.and_then(|s| s.signal()),
            Some(libc::SIGKILL),
            "the drop left the recorded pid alive"
        );
    }
}
