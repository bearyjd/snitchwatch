//! The exit path's sweep result, hard alarm, non-blocking diagnostic and
//! exit status (#329 item 2). In-process tests inject the alarm and the sweep;
//! only the re-exec fixture arms a real alarm, in its own child process.
use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const EXIT_STATUS_CHILD: &str = "SNITCHWATCH_TEST_EXIT_STATUS";

#[test]
fn finish_process_with_returns_the_incomplete_sweep() {
    let _lock = serial();
    let sweep = finish_process_with(|_| {}, || sweep_with(3)).expect("a running exit sweeps");
    assert!(!sweep.complete);
    assert_eq!(sweep.failures, 3);
    assert!(sweep.first_error.is_some());
    assert_eq!(watchdog_state().phase, Phase::Exiting);
}

#[test]
fn an_incomplete_sweep_supersedes_every_status_and_a_clean_one_keeps_it() {
    let incomplete = sweep_with(1);
    let unfinished = crate::containment::Sweep {
        complete: false,
        failures: 0,
        first_error: None,
    };
    let clean = sweep_with(0);
    for code in [0, 1, 2] {
        assert_eq!(exit_code(code, Some(&incomplete)), EXIT_SWEEP_INCOMPLETE);
        assert_eq!(exit_code(code, Some(&unfinished)), EXIT_SWEEP_INCOMPLETE);
        assert_eq!(exit_code(code, Some(&clean)), code);
        assert_eq!(exit_code(code, None), code);
    }
    assert_eq!(EXIT_SWEEP_INCOMPLETE, 3);
}

/// The alarm is armed exactly once, on `Running` -> `Exiting`, before the
/// sweep; never by a later exit, nor after the watchdog's expiry.
#[test]
fn the_exit_alarm_is_armed_once_on_exiting_before_the_sweep() {
    let _lock = serial();
    assert_eq!(EXIT_ALARM_SECS as u64, DESCENDANT_SWEEP.as_secs() + 5);
    let events = std::cell::RefCell::new(Vec::new());
    let arm = |seconds: u32| {
        assert_eq!(
            watchdog_state().phase,
            Phase::Exiting,
            "armed before Exiting"
        );
        events.borrow_mut().push(format!("alarm {seconds}"));
    };
    let sweep = || {
        events.borrow_mut().push("sweep".to_string());
        sweep_with(0)
    };
    assert!(finish_process_with(arm, sweep).is_some());
    let expected = vec![format!("alarm {EXIT_ALARM_SECS}"), "sweep".to_string()];
    assert_eq!(*events.borrow(), expected);
    for _ in 0..2 {
        let again = finish_process_with(
            |_| events.borrow_mut().push("alarm again".into()),
            || {
                events.borrow_mut().push("sweep again".into());
                sweep_with(0)
            },
        );
        assert!(again.is_none(), "an Exiting scanner sweeps no second time");
    }
    assert_eq!(*events.borrow(), expected, "Exiting armed a second alarm");
}

#[test]
fn an_expired_scanner_arms_no_exit_alarm() {
    let _lock = serial();
    // Group 1 is refused after the expiry began: nothing is killed, the
    // phase still ends Expired.
    expire_watchdog(1, || {});
    assert_eq!(watchdog_state().phase, Phase::Expired);
    let armed = std::cell::Cell::new(0);
    let swept = std::cell::Cell::new(0);
    for _ in 0..2 {
        let sweep = finish_process_with(
            |_| armed.set(armed.get() + 1),
            || {
                swept.set(swept.get() + 1);
                sweep_with(0)
            },
        );
        assert!(sweep.is_none());
    }
    assert_eq!((armed.get(), swept.get()), (0, 0));
}

/// A pipe as (read, write), both close-on-exec.
fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: `fds` is a live two-element array, as `pipe2` requires.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: `pipe2` succeeded, so both fds are open and owned only here.
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn set_nonblocking(fd: &OwnedFd, on: bool) {
    // SAFETY: `fcntl` on an fd this test owns; it touches no memory.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0);
        let flags = if on {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        assert_eq!(libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags), 0);
    }
}

/// Fills the pipe to capacity, then leaves its write end BLOCKING again: a
/// write that did not set `O_NONBLOCK` itself would hang on it.
fn fill(write: &OwnedFd) {
    set_nonblocking(write, true);
    for chunk in [4096, 1] {
        let bytes = vec![b'x'; chunk];
        // SAFETY: writes `bytes.len()` bytes from a live buffer to our fd.
        while unsafe { libc::write(write.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) } > 0 {}
    }
    set_nonblocking(write, false);
}

/// Everything currently in the pipe, without blocking.
fn drain(read: &OwnedFd) -> Vec<u8> {
    set_nonblocking(read, true);
    let mut all = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        // SAFETY: reads at most `buffer.len()` bytes into a live buffer.
        let n = unsafe { libc::read(read.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if n <= 0 {
            return all;
        }
        all.extend_from_slice(&buffer[..n as usize]);
    }
}

#[test]
fn a_full_stderr_pipe_does_not_block_the_exit_diagnostic() {
    let (read, write) = pipe();
    fill(&write);
    let fd = write.as_raw_fd();
    let (done, finished) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        write_marker_nonblocking(fd, EXIT_SWEEP_INCOMPLETE_MARKER);
        let _ = done.send(());
    });
    if finished.recv_timeout(Duration::from_secs(5)).is_err() {
        // Unwedge the blocked write (EPIPE; SIGPIPE is ignored) before failing.
        drop(read);
        let _ = writer.join();
        panic!("the exit diagnostic blocked on a full pipe");
    }
    writer.join().unwrap();
    let contents = drain(&read);
    assert!(!contents.is_empty());
    assert!(
        contents.iter().all(|&b| b == b'x'),
        "a partial marker was written"
    );
}

#[test]
fn the_exit_diagnostic_reaches_an_empty_pipe_and_skips_other_files() {
    let (read, write) = pipe();
    write_marker_nonblocking(write.as_raw_fd(), EXIT_SWEEP_INCOMPLETE_MARKER);
    assert_eq!(drain(&read), EXIT_SWEEP_INCOMPLETE_MARKER);
    let file = tempfile::tempfile().unwrap();
    write_marker_nonblocking(file.as_raw_fd(), EXIT_SWEEP_INCOMPLETE_MARKER);
    assert_eq!(file.metadata().unwrap().len(), 0, "wrote to a regular file");
    // An fd that is not open is ignored too.
    write_marker_nonblocking(-1, EXIT_SWEEP_INCOMPLETE_MARKER);
    for marker in [EXIT_SWEEP_INCOMPLETE_MARKER, WATCHDOG_TIMEOUT_MARKER] {
        assert!(marker.len() < libc::PIPE_BUF);
    }
}

/// The child's mode: "<sweep>:<exit>", sweep "clean" (the real sweep, which
/// finds no descendant) or "incomplete" (the fake), exit a run status passed
/// to `exit_after_sweep` or "guard" (an `ExitGuard` dropped, then exit 0).
fn exit_status_child(mode: &str) -> ! {
    let (sweep, exit) = mode.split_once(':').expect("mode is <sweep>:<exit>");
    FAKE_INCOMPLETE_EXIT_SWEEP.store(sweep == "incomplete", Ordering::SeqCst);
    if exit == "guard" {
        {
            let _guard = ExitGuard;
        }
        std::process::exit(0);
    }
    exit_after_sweep(exit.parse().expect("an exit status"))
}

/// Through the real exit paths (`exit_after_sweep`, `ExitGuard`) in a
/// re-executed child: an incomplete sweep exits 3, a clean one keeps 0 or 2.
#[test]
fn an_incomplete_exit_sweep_exits_3_and_a_clean_one_keeps_its_status() {
    if let Some(mode) = std::env::var_os(EXIT_STATUS_CHILD) {
        exit_status_child(mode.to_str().expect("an ASCII mode"));
    }
    let cases = [
        ("clean:0", 0),
        ("clean:2", 2),
        ("clean:guard", 0),
        ("incomplete:0", 3),
        ("incomplete:1", 3),
        ("incomplete:2", 3),
        ("incomplete:guard", 3),
    ];
    for (mode, expected) in cases {
        let mut scanner = ScannerFixture::reexec(
            "command::tests::exit_tests::an_incomplete_exit_sweep_exits_3_and_a_clean_one_keeps_its_status",
            |command, _| {
                command.env(EXIT_STATUS_CHILD, mode);
            },
        );
        let exited = scanner.exited_within(Duration::from_secs(20));
        let (status, problems) = scanner.finish();
        assert!(exited, "{mode}: the fixture never finished");
        assert_eq!(status.code(), Some(expected), "{mode}: {status:?}");
        assert!(
            problems.is_empty(),
            "{mode}: fixture cleanup refused: {problems:?}"
        );
    }
}
