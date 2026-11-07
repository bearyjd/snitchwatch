use super::*;

#[test]
fn unsafe_targets_are_refused_and_never_signalled() {
    for bad in [-1, 0, 1, i32::MIN] {
        let mut called = false;
        assert!(!kill_with(bad, |_| called = true), "{bad}");
        assert!(!called, "signalled {bad}");
        assert!(!kill(bad) && !killpg(bad));
    }
    let mut got = 0;
    assert!(kill_with(2, |p| got = p));
    assert_eq!(got, 2);
}

use std::cell::Cell;
fn null_fd() -> OwnedFd {
    std::fs::File::open("/dev/null").unwrap().into()
}
fn errno(e: i32) -> io::Result<OwnedFd> {
    Err(io::Error::from_raw_os_error(e))
}

#[test]
fn no_pidfd_means_refused_and_nothing_is_sent() {
    for e in [libc::EPERM, libc::ENOSYS, libc::EMFILE] {
        let sent = Cell::new(false);
        let asked = Cell::new(false);
        let got = kill_verified_with(
            4242,
            |_| errno(e),
            |_| {
                sent.set(true);
                Ok(())
            },
            |_| {
                asked.set(true);
                true
            },
        );
        assert_eq!(got, Verified::Refused(e));
        assert!(!sent.get() && !asked.get(), "errno {e}");
    }
}

#[test]
fn a_vanished_process_is_gone() {
    let sent = Cell::new(false);
    let got = kill_verified_with(
        4242,
        |_| errno(libc::ESRCH),
        |_| {
            sent.set(true);
            Ok(())
        },
        |_| true,
    );
    assert_eq!(got, Verified::Gone);
    assert!(!sent.get());
}

#[test]
fn identity_is_checked_after_the_pidfd_is_held_and_gates_the_send() {
    let events = std::cell::RefCell::new(Vec::new());
    let got = kill_verified_with(
        4242,
        |_| {
            events.borrow_mut().push("open");
            Ok(null_fd())
        },
        |_| {
            events.borrow_mut().push("send");
            Ok(())
        },
        |_| {
            events.borrow_mut().push("identity");
            false
        },
    );
    assert_eq!(got, Verified::Mismatch);
    assert_eq!(*events.borrow(), ["open", "identity"]);
}

#[test]
fn a_verified_process_is_signalled_through_the_opened_fd() {
    let fd = null_fd();
    let raw = fd.as_raw_fd();
    let sent_on = Cell::new(-1);
    let got = kill_verified_with(
        4242,
        |_| Ok(fd),
        |f| {
            sent_on.set(f);
            Ok(())
        },
        |_| true,
    );
    assert_eq!(got, Verified::Sent);
    assert_eq!(sent_on.get(), raw);
}

#[test]
fn send_errors_are_gone_for_esrch_and_refused_otherwise() {
    let send = |e: i32| {
        kill_verified_with(
            4242,
            |_| Ok(null_fd()),
            |_| Err(io::Error::from_raw_os_error(e)),
            |_| true,
        )
    };
    assert_eq!(send(libc::ESRCH), Verified::Gone);
    assert_eq!(send(libc::EPERM), Verified::Refused(libc::EPERM));
}

#[test]
fn unsafe_pids_never_reach_open() {
    for bad in [-1, 0, 1, i32::MIN] {
        let opened = Cell::new(false);
        let got = kill_verified_with(
            bad,
            |_| {
                opened.set(true);
                Ok(null_fd())
            },
            |_| Ok(()),
            |_| true,
        );
        assert_eq!(got, Verified::Unsafe, "{bad}");
        assert!(!opened.get(), "{bad}");
    }
}

/// Killed and reaped whatever the test did, before any assertion.
struct Guard(std::process::Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn marked_sleep(marker: &str) -> Guard {
    use std::os::unix::process::CommandExt;
    Guard(
        std::process::Command::new("/usr/bin/sleep")
            .arg0("sleep")
            .arg(marker)
            .spawn()
            .unwrap(),
    )
}

#[test]
fn kill_marked_kills_the_marked_process_and_spares_others() {
    use std::os::unix::process::ExitStatusExt;
    let marker = unique_marker();
    let mut mine = marked_sleep(&marker);
    let mut other = marked_sleep(&unique_marker());
    assert_eq!(
        kill_marked(other.0.id() as i32, &marker),
        Verified::Mismatch
    );
    assert!(other.0.try_wait().unwrap().is_none(), "killed a mismatch");
    assert_eq!(kill_marked(mine.0.id() as i32, &marker), Verified::Sent);
    assert_eq!(mine.0.wait().unwrap().signal(), Some(libc::SIGKILL));
}

#[test]
fn a_mismatch_is_retried_until_it_matches_but_only_so_often() {
    let pause = std::time::Duration::from_millis(1);
    let calls = Cell::new(0);
    let got = retry_mismatch(5, pause, || {
        calls.set(calls.get() + 1);
        if calls.get() < 3 {
            Verified::Mismatch
        } else {
            Verified::Sent
        }
    });
    assert_eq!((got, calls.get()), (Verified::Sent, 3));
    calls.set(0);
    let got = retry_mismatch(4, pause, || {
        calls.set(calls.get() + 1);
        Verified::Mismatch
    });
    assert_eq!((got, calls.get()), (Verified::Mismatch, 4));
    calls.set(0);
    let got = retry_mismatch(4, pause, || {
        calls.set(calls.get() + 1);
        Verified::Refused(libc::EPERM)
    });
    assert_eq!((got, calls.get()), (Verified::Refused(libc::EPERM), 1));
}

/// The race seen on loaded hosts: a fresh process whose exec has not
/// landed carries its parent's cmdline; the marker arrives later.
#[test]
fn a_process_that_execs_late_is_still_killed() {
    use std::os::unix::process::ExitStatusExt;
    let marker = unique_marker();
    let mut child = Guard(
        std::process::Command::new("/usr/bin/sh")
            .args(["-c", "sleep 0.2; exec sleep \"$MARK\""])
            .env("MARK", &marker)
            .spawn()
            .unwrap(),
    );
    let got = kill_marked_patiently(child.0.id() as i32, &marker);
    // Asserted before any wait: the guard kills and reaps on failure.
    assert_eq!(got, Verified::Sent);
    let status = child.0.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

#[test]
fn a_reaped_pid_is_gone_not_signalled() {
    let marker = unique_marker();
    let mut child = marked_sleep(&marker);
    let pid = child.0.id() as i32;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    // Reaped, so the pid is free (and may be reused): never Sent.
    assert_ne!(kill_marked(pid, &marker), Verified::Sent);
}

/// A zombie is dead, not a process that has yet to exec: no retries.
#[test]
fn a_zombie_is_gone_at_once_not_after_the_retries() {
    let marker = unique_marker();
    let mut child = marked_sleep(&marker);
    let pid = child.0.id() as i32;
    child.0.kill().unwrap(); // not reaped: a zombie
    let start = std::time::Instant::now();
    while !crate::containment::parent_and_zombie(pid).is_some_and(|(_, z)| z)
        && start.elapsed() < std::time::Duration::from_secs(3)
    {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let start = std::time::Instant::now();
    let got = kill_marked_patiently(pid, &marker);
    let took = start.elapsed();
    assert_eq!(got, Verified::Gone);
    assert!(took < std::time::Duration::from_millis(100), "{took:?}");
}

fn emfile() -> io::Error {
    io::Error::from_raw_os_error(libc::EMFILE)
}

#[test]
fn an_unreadable_proc_entry_is_refused_not_gone_and_not_retried() {
    let stats = Cell::new(0);
    let attempts = Cell::new(0);
    let got = patiently_with(
        4242,
        |_| {
            stats.set(stats.get() + 1);
            Err(emfile())
        },
        |_| Ok(false),
        || {
            attempts.set(attempts.get() + 1);
            Verified::Sent
        },
    );
    assert_eq!(got, Verified::Refused(libc::EMFILE));
    assert_eq!((stats.get(), attempts.get()), (1, 0));
}

#[test]
fn only_a_vanished_or_fully_dead_zombie_is_gone() {
    let attempts = Cell::new(0);
    let run = |stat: Option<(i32, bool)>, tasks: io::Result<bool>| {
        attempts.set(0);
        let got = patiently_with(
            4242,
            |_| Ok(stat),
            |_| match &tasks {
                Ok(b) => Ok(*b),
                Err(_) => Err(emfile()),
            },
            || {
                attempts.set(attempts.get() + 1);
                Verified::Sent
            },
        );
        (got, attempts.get())
    };
    assert_eq!(run(None, Ok(false)), (Verified::Gone, 0));
    assert_eq!(run(Some((1, true)), Ok(false)), (Verified::Gone, 0));
    // A zombie leader with running threads, or unknown threads: tried.
    assert_eq!(run(Some((1, true)), Ok(true)), (Verified::Sent, 1));
    assert_eq!(run(Some((1, true)), Err(emfile())), (Verified::Sent, 1));
    assert_eq!(run(Some((1, false)), Ok(false)), (Verified::Sent, 1));
}

#[test]
fn only_newline_terminated_pid_lines_count() {
    assert_eq!(complete_pids("11\n22\n"), [11, 22]);
    assert_eq!(complete_pids("11\n22\n3"), [11, 22]);
    assert!(complete_pids("12").is_empty());
    assert!(complete_pids("").is_empty());
    assert_eq!(complete_pids("11\nx\n22\n"), [11, 22]);
}

#[test]
fn a_failed_fork_is_an_error_not_a_pid() {
    assert!(fork_result(-1).is_err());
    assert_eq!(fork_result(0).unwrap(), 0);
    assert_eq!(fork_result(1234).unwrap(), 1234);
}
