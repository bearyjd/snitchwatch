//! Signalling for test fixtures: every kill refuses a pid or group <= 1
//! (`kill(-1)` would signal everything the user may signal, `kill(0)` the
//! caller's own group, 1 is init) so a failed fork's -1 can never reach one.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(crate) fn target(pid: i32) -> Option<i32> {
    (pid > 1).then_some(pid)
}

/// Calls `kill` only for a safe `pid`; whether it did.
pub(crate) fn kill_with(pid: i32, kill: impl FnOnce(i32)) -> bool {
    target(pid).map(kill).is_some()
}

pub(crate) fn kill(pid: i32) -> bool {
    kill_with(pid, |p| unsafe {
        libc::kill(p, libc::SIGKILL);
    })
}

/// SIGKILL the process group `group` (not a negative pid by hand).
pub(crate) fn killpg(group: i32) -> bool {
    kill_with(group, |g| unsafe {
        libc::killpg(g, libc::SIGKILL);
    })
}

/// What [`kill_verified_with`] did.
#[derive(Debug, PartialEq)]
pub(crate) enum Verified {
    /// SIGKILL was sent through the pidfd.
    Sent,
    /// The process was already gone.
    Gone,
    /// The pid is not the process the caller meant; nothing was sent.
    Mismatch,
    /// The pid is <= 1; nothing was opened or sent.
    Unsafe,
    /// No pidfd could be had, or the send failed (errno); nothing was
    /// signalled by pid instead.
    Refused(i32),
}

/// SIGKILL `pid` through a pidfd, after `identity` confirmed it *while the
/// pidfd was held*. Never signals by bare pid: when no pidfd can be had
/// the result is `Refused`. `open` and `send` are injected for tests.
pub(crate) fn kill_verified_with(
    pid: i32,
    open: impl FnOnce(i32) -> io::Result<OwnedFd>,
    send: impl FnOnce(i32) -> io::Result<()>,
    identity: impl FnOnce(i32) -> bool,
) -> Verified {
    if target(pid).is_none() {
        return Verified::Unsafe;
    }
    let fd = match open(pid) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Verified::Gone,
        Err(e) => return Verified::Refused(e.raw_os_error().unwrap_or(libc::EIO)),
    };
    if !identity(pid) {
        return Verified::Mismatch;
    }
    match send(fd.as_raw_fd()) {
        Ok(()) => Verified::Sent,
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => Verified::Gone,
        Err(e) => Verified::Refused(e.raw_os_error().unwrap_or(libc::EIO)),
    }
}

pub(crate) fn open_pidfd(pid: i32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
}

fn send_kill(fd: i32) -> io::Result<()> {
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if sent == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// [`kill_verified_with`] on the real syscalls.
pub(crate) fn kill_verified(pid: i32, identity: impl FnOnce(i32) -> bool) -> Verified {
    kill_verified_with(pid, open_pidfd, send_kill, identity)
}

/// SIGKILL the process group `group` after `identity` confirmed its
/// leader (pid == `group`) while a pidfd for it was held. Only for a
/// leader the caller has not reaped (or that is its own process): its pid
/// then cannot be recycled between the check and the `killpg`.
pub(crate) fn killpg_verified_with(
    group: i32,
    open: impl FnOnce(i32) -> io::Result<OwnedFd>,
    killpg: impl FnOnce(i32),
    identity: impl FnOnce(i32) -> bool,
) -> Verified {
    if target(group).is_none() {
        return Verified::Unsafe;
    }
    let _fd = match open(group) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Verified::Gone,
        Err(e) => return Verified::Refused(e.raw_os_error().unwrap_or(libc::EIO)),
    };
    if !identity(group) {
        return Verified::Mismatch;
    }
    killpg(group);
    Verified::Sent
}

/// [`killpg_verified_with`] on the real syscalls; the leader is the
/// caller's unreaped child (live or zombie), the caller itself, or
/// carries `marker` in its cmdline.
pub(crate) fn killpg_marked(group: i32, marker: &str) -> Verified {
    killpg_verified_with(
        group,
        open_pidfd,
        |g| unsafe {
            libc::killpg(g, libc::SIGKILL);
        },
        |p| {
            has_marker(p, marker)
                || p == unsafe { libc::getpid() }
                || crate::containment::parent_and_zombie(p)
                    .is_some_and(|(parent, _)| parent == unsafe { libc::getpid() })
        },
    )
}

/// SIGKILL `pid` only if its cmdline contains `marker` (a
/// [`unique_marker`]), checked while its pidfd is held.
pub(crate) fn kill_marked(pid: i32, marker: &str) -> Verified {
    kill_verified(pid, |p| has_marker(p, marker))
}

/// Whether `pid`'s cmdline contains `marker`.
pub(crate) fn has_marker(pid: i32, marker: &str) -> bool {
    assert!(!marker.is_empty(), "an empty marker matches everything");
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|c| c.windows(marker.len()).any(|w| w == marker.as_bytes()))
}

/// `attempt` until it is not `Mismatch`, at most `tries` times with
/// `pause` between. A process just spawned has an empty cmdline (new mm,
/// args not yet copied) or its parent's until its exec lands, so a first
/// `Mismatch` is not final; each
/// attempt re-verifies under a fresh pidfd, so a recycled pid still never
/// matches.
pub(crate) fn retry_mismatch(
    tries: u32,
    pause: std::time::Duration,
    mut attempt: impl FnMut() -> Verified,
) -> Verified {
    let mut got = attempt();
    for _ in 1..tries {
        if got != Verified::Mismatch {
            break;
        }
        std::thread::sleep(pause);
        got = attempt();
    }
    got
}

/// [`kill_marked`], patient about a process that has not exec'd yet. A
/// zombie or vanished pid is `Gone` at once: it has nothing left to exec.
pub(crate) fn kill_marked_patiently(pid: i32, marker: &str) -> Verified {
    patiently_with(
        pid,
        crate::containment::read_stat,
        crate::containment::has_other_tasks,
        || kill_marked(pid, marker),
    )
}

/// The retry policy of [`kill_marked_patiently`] with `/proc` readers and
/// the kill attempt injected. `Gone` only for a pid that is vanished or a
/// zombie with no other task left (a zombie leader with running threads
/// still dies of SIGKILL). An unreadable `/proc` entry is `Refused`
/// (EMFILE, EACCES, hidepid...), never `Gone`, and is not retried.
pub(crate) fn patiently_with(
    pid: i32,
    stat: impl Fn(i32) -> io::Result<Option<(i32, bool)>>,
    tasks: impl Fn(i32) -> io::Result<bool>,
    mut attempt: impl FnMut() -> Verified,
) -> Verified {
    retry_mismatch(50, std::time::Duration::from_millis(10), || {
        match stat(pid) {
            Err(e) => Verified::Refused(e.raw_os_error().unwrap_or(libc::EIO)),
            Ok(None) => Verified::Gone,
            Ok(Some((_, true))) if !tasks(pid).unwrap_or(true) => Verified::Gone,
            Ok(_) => attempt(),
        }
    })
}

/// The pids of the newline-terminated lines of a fixture pid file; an
/// unterminated last line may be a torn write and is ignored.
pub(crate) fn complete_pids(text: &str) -> Vec<i32> {
    let Some((complete, _torn_tail)) = text.rsplit_once('\n') else {
        return Vec::new();
    };
    complete
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// A marker no other test invocation shares: pid, clock and a counter.
pub(crate) fn unique_marker() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("29.{}{:09}{n}", unsafe { libc::getpid() }, nanos)
}

/// Whether `pid` is gone or only a zombie; an unreadable `/proc` entry
/// (after a few tries) is an error, never "dead".
pub(crate) fn dead_with(
    pid: i32,
    stat: impl Fn(i32) -> io::Result<Option<(i32, bool)>>,
) -> io::Result<bool> {
    let mut last = None;
    for _ in 0..5 {
        match stat(pid) {
            Ok(seen) => return Ok(seen.is_none_or(|(_, zombie)| zombie)),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Err(last.expect("at least one try"))
}

/// [`dead_with`] on the real `/proc`; panics loudly on an unreadable entry
/// so a test cannot pass on a failed observation.
pub(crate) fn dead(pid: i32) -> bool {
    dead_with(pid, crate::containment::read_stat)
        .unwrap_or_else(|e| panic!("cannot tell whether pid {pid} is dead: {e}"))
}

/// `/proc/<pid>/stat` as (parent, zombie), `None` when vanished.
pub(crate) fn proc_stat(pid: i32) -> io::Result<Option<(i32, bool)>> {
    crate::containment::read_stat(pid)
}

/// A raw `fork()` result as a pid, or the OS error.
pub(crate) fn fork_result(result: i32) -> io::Result<i32> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
