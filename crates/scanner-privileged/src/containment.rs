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
/// Read as bytes: a comm need not be valid UTF-8, and such a process must
/// still be found.
pub(crate) fn parent_and_zombie(pid: i32) -> Option<(i32, bool)> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses: split at the last ')'.
    let tail = &stat[stat.iter().rposition(|&b| b == b')')? + 1..];
    let mut fields = tail
        .split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty());
    let state = fields.next()?;
    let parent = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    Some((parent, state == b"Z"))
}

/// A zombie thread-group leader whose other threads still run is alive: only
/// a leader with no other task left is dead.
fn has_other_tasks(pid: i32) -> bool {
    std::fs::read_dir(format!("/proc/{pid}/task")).is_ok_and(|tasks| {
        tasks
            .flatten()
            .any(|task| task.file_name().to_str() != Some(&pid.to_string()))
    })
}

/// Every live process below `root`, by the current `/proc` (a zombie is dead
/// unless threads of its group still run).
fn live_descendants(root: i32, deadline: Instant) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut children: HashMap<i32, Vec<(i32, bool)>> = HashMap::new();
    for entry in entries.flatten() {
        // A pass over a huge /proc must not outlast the bound.
        if Instant::now() >= deadline {
            return Vec::new();
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some((parent, zombie)) = parent_and_zombie(pid) {
            let dead = zombie && !has_other_tasks(pid);
            children.entry(parent).or_default().push((pid, dead));
        }
    }
    let mut live = Vec::new();
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        for &(pid, dead) in children.get(&parent).into_iter().flatten() {
            if seen.insert(pid) {
                if !dead {
                    live.push(pid);
                }
                // Walk through dead nodes too: a snapshot can list a child
                // under a parent that is already exiting.
                stack.push(pid);
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

/// What a [`kill_descendants`] sweep did. Errors are only counted (plus the
/// first kept), never logged: the caller reports them after its final kill.
#[derive(Debug)]
pub struct Sweep {
    /// A pass found no live descendant before the bound.
    pub complete: bool,
    /// Signals that failed for a reason other than the process being gone.
    pub failures: u32,
    pub first_error: Option<io::Error>,
}

/// Kill every process below `root` (the scanner), pass after pass, until two
/// consecutive passes find none alive or `bound` has passed. Two, because one
/// `/proc` snapshot can miss an orphan (a child listed before its parent, the
/// parent then exiting). The bound is checked per `/proc` entry and per pid.
pub fn kill_descendants(root: i32, bound: Duration) -> Sweep {
    let deadline = Instant::now() + bound;
    kill_descendants_with(
        root,
        bound,
        || live_descendants(root, deadline),
        kill_if_descendant,
    )
}

/// [`kill_descendants`] with the `/proc` scan and the kill injected.
fn kill_descendants_with(
    root: i32,
    bound: Duration,
    mut scan: impl FnMut() -> Vec<i32>,
    mut kill: impl FnMut(i32, &HashSet<i32>) -> io::Result<()>,
) -> Sweep {
    let deadline = Instant::now() + bound;
    let mut sweep = Sweep {
        complete: false,
        failures: 0,
        first_error: None,
    };
    let mut empty_passes = 0;
    while Instant::now() < deadline {
        let live = scan();
        if live.is_empty() {
            empty_passes += 1;
            if empty_passes == 2 {
                sweep.complete = true;
                return sweep;
            }
        } else {
            empty_passes = 0;
            let tree: HashSet<i32> = live.iter().copied().chain([root]).collect();
            for &pid in &live {
                if Instant::now() >= deadline {
                    return sweep;
                }
                if let Err(e) = kill(pid, &tree) {
                    // Kept in memory: the watchdog must not log before its kill.
                    sweep.failures += 1;
                    sweep.first_error.get_or_insert(e);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    sweep
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
        assert!(kill_descendants(root.0.id() as i32, Duration::from_secs(3)).complete);
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

    /// Forks a root process `P` and, below it, a `C` whose comm is not valid
    /// UTF-8; returns `(P, C)`. Both pause forever until killed.
    fn fork_root_with_binary_comm() -> (i32, i32) {
        let mut pipe = [0; 2];
        unsafe {
            assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
            let root = libc::fork();
            assert!(root >= 0);
            if root == 0 {
                // Async-signal-safe calls only (forked from a threaded test).
                let child = libc::fork();
                if child == 0 {
                    libc::prctl(libc::PR_SET_NAME, c"\xff\xfex".as_ptr());
                    loop {
                        libc::pause();
                    }
                }
                libc::write(pipe[1], (&child as *const i32).cast(), 4);
                loop {
                    libc::pause();
                }
            }
            libc::close(pipe[1]);
            let mut child: i32 = 0;
            assert_eq!(libc::read(pipe[0], (&mut child as *mut i32).cast(), 4), 4);
            libc::close(pipe[0]);
            (root, child)
        }
    }

    #[test]
    fn a_process_with_a_non_utf8_name_is_found_and_killed() {
        let (root, child) = fork_root_with_binary_comm();
        // Wait until the child has set its name.
        let start = Instant::now();
        while std::fs::read(format!("/proc/{child}/stat"))
            .map(|stat| !stat.windows(3).any(|w| w == b"\xff\xfex"))
            .unwrap_or(true)
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let swept = kill_descendants(root, Duration::from_secs(3));
        // Read as bytes here: the code under test must not be its own oracle.
        let alive = std::fs::read(format!("/proc/{child}/stat")).is_ok_and(|stat| {
            let tail = &stat[stat.iter().rposition(|&b| b == b')').unwrap() + 1..];
            tail.trim_ascii_start().first() != Some(&b'Z')
        });
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::kill(root, libc::SIGKILL);
            libc::waitpid(root, std::ptr::null_mut(), 0);
        }
        assert!(swept.complete);
        assert!(!alive, "the non-UTF-8-named descendant survived");
    }

    #[test]
    fn completion_needs_two_consecutive_empty_passes() {
        // An empty snapshot followed by a non-empty one (an orphan the first
        // snapshot missed) must not complete the sweep.
        let mut snapshots = vec![vec![], vec![7], vec![], vec![]].into_iter();
        let mut scans = 0;
        let mut killed = Vec::new();
        let sweep = kill_descendants_with(
            1,
            Duration::from_secs(5),
            || {
                scans += 1;
                snapshots.next().unwrap_or_default()
            },
            |pid, _| {
                killed.push(pid);
                Ok(())
            },
        );
        assert!(sweep.complete);
        assert_eq!(killed, [7]);
        assert_eq!(scans, 4);
    }

    #[test]
    fn the_deadline_is_checked_per_pid() {
        let mut kills = 0;
        let sweep = kill_descendants_with(
            1,
            Duration::from_millis(50),
            || vec![2, 3, 4, 5, 6, 7, 8, 9],
            |_, _| {
                kills += 1;
                std::thread::sleep(Duration::from_millis(40));
                Ok(())
            },
        );
        assert!(!sweep.complete);
        assert!(kills <= 3, "kept signalling past the bound: {kills}");
    }

    extern "C" fn pause_forever(_: *mut libc::c_void) -> *mut libc::c_void {
        loop {
            unsafe { libc::pause() };
        }
    }

    /// Forks a root `P` and, below it, a `C` whose thread-group leader exits
    /// (SYS_exit) while a second thread keeps running; returns `(P, C)`.
    fn fork_root_with_leaderless_thread_group() -> (i32, i32) {
        let mut pipe = [0; 2];
        unsafe {
            assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
            let root = libc::fork();
            assert!(root >= 0);
            if root == 0 {
                let child = libc::fork();
                if child == 0 {
                    // The forking thread is the new process's leader.
                    let mut thread: libc::pthread_t = 0;
                    libc::pthread_create(
                        &mut thread,
                        std::ptr::null(),
                        pause_forever,
                        std::ptr::null_mut(),
                    );
                    libc::syscall(libc::SYS_exit, 0);
                }
                libc::write(pipe[1], (&child as *const i32).cast(), 4);
                loop {
                    libc::pause();
                }
            }
            libc::close(pipe[1]);
            let mut child: i32 = 0;
            assert_eq!(libc::read(pipe[0], (&mut child as *mut i32).cast(), 4), 4);
            libc::close(pipe[0]);
            (root, child)
        }
    }

    /// Tasks of `pid` besides its leader entry, read straight from /proc.
    fn other_tasks(pid: i32) -> usize {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .map(|dir| {
                dir.flatten()
                    .filter(|e| e.file_name().to_str() != Some(&pid.to_string()))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn a_zombie_leader_with_running_threads_is_found_and_killed() {
        let (root, child) = fork_root_with_leaderless_thread_group();
        // Wait for the leader to be a zombie with its other thread running.
        let start = Instant::now();
        while !(parent_and_zombie(child).is_some_and(|(_, zombie)| zombie)
            && other_tasks(child) > 0)
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let setup = other_tasks(child) > 0;
        let swept = kill_descendants(root, Duration::from_secs(3));
        let start = Instant::now();
        while other_tasks(child) > 0 && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let survived = other_tasks(child) > 0;
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::kill(root, libc::SIGKILL);
            libc::waitpid(root, std::ptr::null_mut(), 0);
        }
        assert!(setup, "the fixture never reached its leaderless state");
        assert!(swept.complete);
        assert!(!survived, "a thread group with a zombie leader survived");
    }
}
