//! Watchdog containment for the root scanner's descendants.
//!
//! The scanner is a child subreaper (`PR_SET_CHILD_SUBREAPER`), so every
//! process a primitive forks stays in the scanner's subtree even after the
//! primitive dies: orphans reparent to the scanner, not to init. On watchdog
//! expiry [`kill_descendants`] walks that subtree in `/proc` and SIGKILLs each
//! live process through a pidfd, repeating until a pass finds none alive (a
//! process killed between passes cannot fork again; one that forked before
//! its kill shows its child on the next pass). Only then does the watchdog
//! kill the scanner's own group. This does not depend on the primitive
//! registry, its lock, or the capture thread running again.
//!
//! Chosen over a cgroup: a scope needs a delegated cgroup subtree, which a
//! pkexec-launched root helper is not given (it runs in the caller's session
//! scope, with no systemd-run or Delegate=), and creating one would add the
//! persistent privileged machinery the scanner's design rules out.

use std::collections::{HashMap, HashSet};
use std::io;
use std::time::{Duration, Instant};

/// Keep orphaned descendants in the scanner's subtree. Called once at
/// startup, before any primitive runs.
pub fn become_subreaper() -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One `/proc/<pid>/stat` line's parent and whether the process is a zombie.
fn parent_and_zombie(pid: i32) -> Option<(i32, bool)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses: split at the last ')'.
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    Some((parent, state == "Z"))
}

/// Every live (non-zombie) process below `root`, by the current `/proc`.
fn live_descendants(root: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut children: HashMap<i32, Vec<(i32, bool)>> = HashMap::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some((parent, zombie)) = parent_and_zombie(pid) {
            children.entry(parent).or_default().push((pid, zombie));
        }
    }
    let mut live = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        for &(pid, zombie) in children.get(&parent).into_iter().flatten() {
            if seen.insert(pid) {
                // A zombie's children were already reparented; nothing to walk.
                if !zombie {
                    live.push(pid);
                    stack.push(pid);
                }
            }
        }
    }
    live
}

/// `pidfd_open(pid)`, as a raw fd or the OS error.
fn open_pidfd(pid: i32) -> io::Result<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd as i32)
    }
}

/// SIGKILL `pid` only if, once pinned by a pidfd, it is still a child of one
/// of `tree` (the scanner or a process found below it). A pid reused by an
/// unrelated process before the pidfd opened fails that check; one reused
/// after it can't be signalled through the pidfd, which names the original.
/// A process that is already gone (`ESRCH`) is skipped, not an error.
fn kill_if_descendant(pid: i32, tree: &HashSet<i32>) -> io::Result<()> {
    kill_if_descendant_with(pid, tree, open_pidfd)
}

/// [`kill_if_descendant`] with the pidfd opener injected, so the fallback is
/// testable.
fn kill_if_descendant_with(
    pid: i32,
    tree: &HashSet<i32>,
    open: impl FnOnce(i32) -> io::Result<i32>,
) -> io::Result<()> {
    let in_tree = || parent_and_zombie(pid).is_some_and(|(parent, _)| tree.contains(&parent));
    let fd = match open(pid) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
        Err(_) => {
            // Fail safe when no pidfd can be had (kernel before 5.3, EMFILE,
            // a seccomp filter...): kill by pid after the same parent check,
            // accepting the reuse window pidfds close.
            if in_tree() && unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                return ignore_gone(io::Error::last_os_error());
            }
            return Ok(());
        }
    };
    let result = if in_tree() {
        let sent = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if sent != 0 {
            ignore_gone(io::Error::last_os_error())
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    unsafe {
        libc::close(fd);
    }
    result
}

/// `ESRCH` (already dead) is success; any other error is surfaced.
fn ignore_gone(e: io::Error) -> io::Result<()> {
    if e.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(e)
    }
}

/// Kill every process below `root` (the scanner), pass after pass, until a
/// pass finds none alive or `bound` has passed. Returns whether it got there.
pub fn kill_descendants(root: i32, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        let live = live_descendants(root);
        if live.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        let tree: HashSet<i32> = live.iter().copied().chain([root]).collect();
        for &pid in &live {
            if let Err(e) = kill_if_descendant(pid, &tree) {
                tracing::warn!(pid, error = %e, "watchdog could not kill a descendant");
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    /// A child that is always killed and reaped, whatever the test did.
    struct Reaped(Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn sleeper() -> Reaped {
        Reaped(Command::new("/usr/bin/sleep").arg("30").spawn().unwrap())
    }
    fn alive(child: &mut Reaped) -> bool {
        child.0.try_wait().unwrap().is_none()
    }
    fn wait_dead(child: &mut Reaped) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if !alive(child) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
    fn me() -> i32 {
        unsafe { libc::getpid() }
    }

    #[test]
    fn sweep_kills_descendants_but_spares_unrelated_processes() {
        // `root` stands in for the scanner; its sleeping child is a
        // descendant. `bystander` is our child, outside root's subtree.
        let mut root = Reaped(
            Command::new("/usr/bin/sh")
                .args(["-c", "sleep 30 & echo $!; read _"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        BufReader::new(root.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: i32 = line.trim().parse().unwrap();
        let mut bystander = sleeper();
        assert!(kill_descendants(root.0.id() as i32, Duration::from_secs(3)));
        let start = Instant::now();
        while std::path::Path::new(&format!("/proc/{descendant}")).exists()
            && !parent_and_zombie(descendant).is_some_and(|(_, z)| z)
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            parent_and_zombie(descendant).is_none_or(|(_, zombie)| zombie),
            "descendant {descendant} survived the sweep"
        );
        assert!(alive(&mut root), "the root itself is not swept");
        assert!(alive(&mut bystander), "a non-descendant was killed");
    }

    #[test]
    fn parent_check_rejects_a_pid_outside_the_tree() {
        let mut outsider = sleeper();
        let pid = outsider.0.id() as i32;
        // Its parent (this test process) is not in the tree.
        kill_if_descendant(pid, &HashSet::from([1])).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(alive(&mut outsider));
        // Control: with the parent in the tree the same call kills it.
        kill_if_descendant(pid, &HashSet::from([me()])).unwrap();
        assert!(wait_dead(&mut outsider));
    }

    #[test]
    fn any_pidfd_failure_but_esrch_falls_back_to_a_parent_checked_kill() {
        for errno in [libc::ENOSYS, libc::EPERM, libc::EMFILE] {
            let mut child = sleeper();
            let pid = child.0.id() as i32;
            // Outside the tree: the fallback must still check the parent.
            kill_if_descendant_with(pid, &HashSet::from([1]), |_| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap();
            std::thread::sleep(Duration::from_millis(50));
            assert!(alive(&mut child), "errno {errno}: killed outside the tree");
            kill_if_descendant_with(pid, &HashSet::from([me()]), |_| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap();
            assert!(wait_dead(&mut child), "errno {errno}: no fallback kill");
        }
    }

    #[test]
    fn pidfd_esrch_means_gone_and_skips_the_kill() {
        let mut child = sleeper();
        kill_if_descendant_with(child.0.id() as i32, &HashSet::from([me()]), |_| {
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(alive(&mut child));
    }

    #[test]
    fn a_failed_signal_is_surfaced() {
        let mut child = sleeper();
        let pid = child.0.id() as i32;
        // A pidfd of the wrong kind (a plain fd) makes pidfd_send_signal fail.
        let bogus = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
        assert!(bogus >= 0);
        let err = kill_if_descendant_with(pid, &HashSet::from([me()]), |_| Ok(bogus)).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EBADF));
        assert!(alive(&mut child));
    }
}
