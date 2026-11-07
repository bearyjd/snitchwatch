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

/// Whether an error only means the process vanished (ENOENT/ESRCH); any other
/// (EMFILE, ENFILE, EACCES, EIO...) says nothing about the process.
fn vanished(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ESRCH))
}

/// One `/proc/<pid>/stat` line's parent and whether the process is a zombie.
/// Read as bytes: a comm need not be valid UTF-8, and such a process must
/// still be found. `Ok(None)`: the process vanished; `Err`: unknown.
fn read_stat(pid: i32) -> io::Result<Option<(i32, bool)>> {
    let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if vanished(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "malformed /proc stat");
    // The command name may contain spaces and parentheses: split at the last ')'.
    let end = stat
        .iter()
        .rposition(|&b| b == b')')
        .ok_or_else(malformed)?;
    let mut fields = stat[end + 1..]
        .split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty());
    let state = fields.next().ok_or_else(malformed)?;
    let parent = std::str::from_utf8(fields.next().ok_or_else(malformed)?)
        .ok()
        .and_then(|p| p.parse().ok())
        .ok_or_else(malformed)?;
    Ok(Some((parent, state == b"Z")))
}

/// [`read_stat`] with "vanished or unreadable" as `None`.
#[cfg(test)]
pub(crate) fn parent_and_zombie(pid: i32) -> Option<(i32, bool)> {
    read_stat(pid).ok().flatten()
}

/// A zombie thread-group leader whose other threads still run is alive: only
/// a leader with no other task left is dead. An unreadable task list is
/// unknown, not dead.
fn has_other_tasks(pid: i32) -> io::Result<bool> {
    let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(e) if vanished(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    for task in tasks {
        match task {
            Ok(task) if task.file_name().to_str() != Some(&pid.to_string()) => return Ok(true),
            Ok(_) => {}
            Err(e) if vanished(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// Every live process below `root`, by the current `/proc` (a zombie is dead
/// unless threads of its group still run). `Ok(None)`: the scan hit `deadline`
/// and is incomplete; `Err`: `/proc` could not be read. Neither says "none".
fn live_descendants(root: i32, deadline: Instant) -> io::Result<Option<Vec<i32>>> {
    let entries = std::fs::read_dir("/proc")?;
    let mut children: HashMap<i32, Vec<(i32, bool)>> = HashMap::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if vanished(&e) => continue,
            Err(e) => return Err(e),
        };
        // A pass over a huge /proc must not outlast the bound.
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if let Some((parent, zombie)) = read_stat(pid)? {
            let dead = zombie && !has_other_tasks(pid)?;
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
    Ok(Some(live))
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
    // A vanished process is simply not ours to kill; an unreadable one is an error.
    let in_tree = || -> io::Result<bool> {
        Ok(read_stat(pid)?.is_some_and(|(parent, _)| tree.contains(&parent)))
    };
    let fd = match open(pid) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
        Err(_) => {
            // Fail safe when no pidfd can be had (kernel before 5.3, EMFILE,
            // a seccomp filter...): kill by pid after the same parent check,
            // accepting the reuse window pidfds close.
            if in_tree()? && unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                return ignore_gone(io::Error::last_os_error());
            }
            return Ok(());
        }
    };
    let result = match in_tree() {
        Ok(true) => {
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
        }
        Ok(false) => Ok(()),
        Err(e) => Err(e),
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
/// parent then exiting). A pass that failed or was cut short by the deadline
/// is no evidence of "none". One deadline covers scans and signalling alike.
pub fn kill_descendants(root: i32, bound: Duration) -> Sweep {
    let deadline = Instant::now() + bound;
    kill_descendants_with(
        root,
        deadline,
        || live_descendants(root, deadline),
        kill_if_descendant,
    )
}

/// [`kill_descendants`] with the `/proc` scan and the kill injected.
fn kill_descendants_with(
    root: i32,
    deadline: Instant,
    mut scan: impl FnMut() -> io::Result<Option<Vec<i32>>>,
    mut kill: impl FnMut(i32, &HashSet<i32>) -> io::Result<()>,
) -> Sweep {
    let mut sweep = Sweep {
        complete: false,
        failures: 0,
        first_error: None,
    };
    let mut empty_passes = 0;
    while Instant::now() < deadline {
        match scan() {
            // Kept in memory: the watchdog must not log before its kill.
            Err(e) => {
                empty_passes = 0;
                sweep.failures += 1;
                sweep.first_error.get_or_insert(e);
            }
            Ok(None) => empty_passes = 0,
            Ok(Some(live)) if live.is_empty() => {
                empty_passes += 1;
                if empty_passes == 2 {
                    sweep.complete = true;
                    return sweep;
                }
            }
            Ok(Some(live)) => {
                empty_passes = 0;
                let tree: HashSet<i32> = live.iter().copied().chain([root]).collect();
                for &pid in &live {
                    if Instant::now() >= deadline {
                        return sweep;
                    }
                    if let Err(e) = kill(pid, &tree) {
                        sweep.failures += 1;
                        sweep.first_error.get_or_insert(e);
                    }
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
    use std::os::unix::process::CommandExt;
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

    /// A forked root `P` with a forked `C` below it; both are killed and
    /// `P` reaped when dropped, so a failed assertion cannot leak them.
    struct ForkedTree {
        root: i32,
        child: i32,
    }
    impl Drop for ForkedTree {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.child, libc::SIGKILL);
                libc::kill(self.root, libc::SIGKILL);
                libc::waitpid(self.root, std::ptr::null_mut(), 0);
            }
        }
    }

    /// Forks `P`, and `P` forks `C`, which runs `body`. The children never
    /// exec, so they close every fd but stdio at once (those would be the
    /// pipes of concurrent tests, kept open past their EOF). Only
    /// async-signal-safe calls run in the children (forked from a threaded
    /// test), `body` included.
    fn fork_tree(body: fn()) -> ForkedTree {
        let close_inherited = || unsafe {
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
        };
        let mut pipe = [0; 2];
        unsafe {
            assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
            let root = libc::fork();
            assert!(root >= 0);
            if root == 0 {
                let child = libc::fork();
                if child == 0 {
                    close_inherited();
                    body();
                    loop {
                        libc::pause();
                    }
                }
                libc::write(pipe[1], (&child as *const i32).cast(), 4);
                close_inherited();
                loop {
                    libc::pause();
                }
            }
            libc::close(pipe[1]);
            let mut child: i32 = 0;
            assert_eq!(libc::read(pipe[0], (&mut child as *mut i32).cast(), 4), 4);
            libc::close(pipe[0]);
            ForkedTree { root, child }
        }
    }

    fn set_binary_name() {
        unsafe {
            libc::prctl(libc::PR_SET_NAME, c"\xff\xfex".as_ptr());
        }
    }

    #[test]
    fn a_process_with_a_non_utf8_name_is_found_and_killed() {
        let tree = fork_tree(set_binary_name);
        let (root, child) = (tree.root, tree.child);
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
            Instant::now() + Duration::from_secs(5),
            || {
                scans += 1;
                Ok(Some(snapshots.next().unwrap_or_default()))
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

    /// A failed or truncated scan is no evidence of "none": it neither counts
    /// as an empty pass nor lets two empty passes straddle it.
    #[test]
    fn failed_or_truncated_scans_never_count_as_empty() {
        let scans: Vec<io::Result<Option<Vec<i32>>>> = vec![
            Ok(Some(vec![])),
            Ok(None),
            Ok(Some(vec![])),
            Err(io::Error::from_raw_os_error(libc::EMFILE)),
            Ok(Some(vec![])),
            Ok(Some(vec![])),
        ];
        let mut scans = scans.into_iter();
        let mut count = 0;
        let sweep = kill_descendants_with(
            1,
            Instant::now() + Duration::from_secs(5),
            || {
                count += 1;
                scans.next().unwrap_or(Ok(None))
            },
            |_, _| Ok(()),
        );
        assert!(sweep.complete);
        assert_eq!(count, 6, "completed on a pair that straddled a bad scan");
        assert_eq!(sweep.failures, 1);
        assert_eq!(
            sweep.first_error.unwrap().raw_os_error(),
            Some(libc::EMFILE)
        );
        // Only failures and truncations: never complete.
        let sweep = kill_descendants_with(
            1,
            Instant::now() + Duration::from_millis(60),
            || Ok(None),
            |_, _| Ok(()),
        );
        assert!(!sweep.complete);
        let sweep = kill_descendants_with(
            1,
            Instant::now() + Duration::from_millis(60),
            || Err(io::Error::from_raw_os_error(libc::EIO)),
            |_, _| Ok(()),
        );
        assert!(!sweep.complete && sweep.failures > 0);
    }

    #[test]
    fn a_scan_past_its_deadline_is_truncated_not_empty() {
        let past = Instant::now() - Duration::from_secs(1);
        assert!(live_descendants(me(), past).unwrap().is_none());
    }

    #[test]
    fn the_deadline_is_checked_per_pid() {
        let mut kills = 0;
        let sweep = kill_descendants_with(
            1,
            Instant::now() + Duration::from_millis(50),
            || Ok(Some(vec![2, 3, 4, 5, 6, 7, 8, 9])),
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

    /// The leader exits (SYS_exit) while a second thread keeps running.
    fn exit_the_leader_keeping_a_thread() {
        unsafe {
            let mut thread: libc::pthread_t = 0;
            libc::pthread_create(
                &mut thread,
                std::ptr::null(),
                pause_forever,
                std::ptr::null_mut(),
            );
            libc::syscall(libc::SYS_exit, 0);
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
        let tree = fork_tree(exit_the_leader_keeping_a_thread);
        let (root, child) = (tree.root, tree.child);
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
        assert!(setup, "the fixture never reached its leaderless state");
        assert!(swept.complete);
        assert!(!survived, "a thread group with a zombie leader survived");
    }

    /// Open fds of `pid` other than stdio, straight from /proc.
    fn extra_fds(pid: i32) -> Vec<String> {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .map(|dir| {
                dir.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|fd| !["0", "1", "2"].contains(&fd.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The raw-fork fixtures never exec, so any fd they inherit (the pipes of
    /// concurrent tests' `capture`) would stay open and block those tests'
    /// EOF waits: they must close everything but stdio.
    #[test]
    fn forked_fixtures_do_not_hold_the_test_binarys_fds() {
        let tree = fork_tree(set_binary_name);
        let (root, child) = (tree.root, tree.child);
        let start = Instant::now();
        while (!extra_fds(root).is_empty() || !extra_fds(child).is_empty())
            && start.elapsed() < Duration::from_secs(2)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let (root_fds, child_fds) = (extra_fds(root), extra_fds(child));
        assert!(root_fds.is_empty(), "root holds {root_fds:?}");
        assert!(child_fds.is_empty(), "child holds {child_fds:?}");
    }

    /// With one fd slot left, `read_dir("/proc")` takes it and every per-pid
    /// read then fails with EMFILE: that must fail the scan, not read as "no
    /// descendants". The limit is lowered in a fixture process, not here.
    #[test]
    fn an_exhausted_fd_table_fails_the_scan_instead_of_emptying_it() {
        use std::process::Command;
        const CHILD: &str = "SNITCHWATCH_TEST_FD_EXHAUSTED";
        if std::env::var_os(CHILD).is_some() {
            let own = me();
            #[allow(clippy::zombie_processes)] // killed with the fixture's group
            let _descendant = Command::new("/usr/bin/sleep").arg("60").spawn().unwrap();
            unsafe {
                // The lowest free fd is the only one the limit may leave.
                let lowest = libc::dup(0);
                libc::close(lowest);
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit);
                limit.rlim_cur = lowest as libc::rlim_t + 1;
                assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
            }
            // An unreadable task list is unknown, not "dead".
            let held = unsafe { libc::dup(0) }; // the last free slot
            let tasks = has_other_tasks(own);
            unsafe {
                libc::close(held);
            }
            let scan = live_descendants(own, Instant::now() + Duration::from_secs(1));
            let sweep = kill_descendants(own, Duration::from_millis(600));
            std::process::exit(if tasks.is_ok() {
                7
            } else if scan.is_ok() {
                3
            } else if sweep.complete {
                5
            } else if sweep.failures == 0 {
                6
            } else {
                0
            });
        }
        let mut fixture = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "containment::tests::an_exhausted_fd_table_fails_the_scan_instead_of_emptying_it",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(status) = fixture.try_wait().unwrap() {
                break Some(status);
            }
            if start.elapsed() > Duration::from_secs(20) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Group kill first (the sleeper is in it), then reap.
        unsafe {
            libc::kill(-(fixture.id() as i32), libc::SIGKILL);
        }
        let _ = fixture.wait();
        let status = status.expect("the fixture never finished");
        assert_eq!(
            status.code(),
            Some(0),
            "3: the scan came back Ok; 5: complete; 6: no failure counted ({status:?})"
        );
    }

    /// A marker no other test invocation shares: pid, clock and a counter.
    fn unique_marker() -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("29.{}{:09}{n}", me(), nanos)
    }

    /// The live `sleep <marker>` processes below `root` only: a process
    /// elsewhere with the same argv is never counted. On drop the whole
    /// subtree of `root` (the fixture's own) is swept, nothing else.
    struct MarkedSleeps {
        marker: String,
        root: i32,
    }
    impl MarkedSleeps {
        /// One observation. A truncated or failed scan, or a cmdline that
        /// cannot be read for any reason but the process having vanished, is
        /// an error: it says nothing about survivors.
        fn try_alive(&self, deadline: Instant) -> io::Result<Vec<i32>> {
            let want = format!("sleep\0{}\0", self.marker).into_bytes();
            let below = live_descendants(self.root, deadline)?
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
            Ok(found)
        }

        /// [`Self::try_alive`], retried a bounded number of times; then the
        /// test fails loudly rather than reading "none".
        fn alive(&self) -> Vec<i32> {
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
    impl Drop for MarkedSleeps {
        fn drop(&mut self) {
            kill_descendants(self.root, Duration::from_secs(5));
        }
    }

    #[test]
    fn a_failed_observation_is_an_error_not_an_empty_list() {
        let fixture = MarkedSleeps {
            marker: unique_marker(),
            root: me(),
        };
        let past = Instant::now() - Duration::from_secs(1);
        assert!(fixture.try_alive(past).is_err());
        // Killing nothing: `me()` has no marked sleepers below it.
        std::mem::forget(fixture);
    }

    #[test]
    fn markers_are_unique_per_call() {
        let (a, b) = (unique_marker(), unique_marker());
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("29.{}", me())));
    }

    #[test]
    fn a_foreign_process_with_the_same_argv_is_neither_counted_nor_killed() {
        let mut fixture_root = Reaped(
            Command::new("/usr/bin/sh")
                .args(["-c", "read _"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let marker = MarkedSleeps {
            marker: "29.7316".into(),
            root: fixture_root.0.id() as i32,
        };
        // Same argv, but not below the fixture's root.
        let mut decoy = Reaped(
            Command::new("/usr/bin/sleep")
                .arg0("sleep")
                .arg("29.7316")
                .spawn()
                .unwrap(),
        );
        assert!(marker.alive().is_empty(), "counted a foreign process");
        drop(marker);
        assert!(alive(&mut decoy), "cleanup killed a foreign process");
        assert!(alive(&mut fixture_root));
    }

    /// Regression guard of the existing sweep, not a proof of any new
    /// mechanism: a shell in its own session forks `sleep`s as fast as it
    /// can (bounded to 300 so a broken sweep cannot exhaust the host) under a
    /// subreaper `root`, like the scanner. Orphans stay in `root`'s subtree
    /// and the next pass finds them, so all end up dead.
    #[test]
    fn a_session_forking_flat_out_is_fully_dead_after_the_sweep() {
        let name = unique_marker();
        let mut command = Command::new("/usr/bin/sh");
        // Like the scanner, `root` adopts orphans: they stay in its subtree.
        unsafe {
            command.pre_exec(become_subreaper);
        }
        let mut root = Reaped(
            command
                .args([
                    "-c",
                    &format!("setsid sh -c 'n=0; while [ $n -lt 300 ]; do sleep {name} & n=$((n+1)); done; wait' & read _"),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let marker = MarkedSleeps {
            marker: name,
            root: root.0.id() as i32,
        };
        let start = Instant::now();
        while marker.alive().len() < 20 && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            marker.alive().len() >= 20,
            "the fixture never started forking"
        );
        let sweep = kill_descendants(root.0.id() as i32, Duration::from_secs(10));
        let start = Instant::now();
        while !marker.alive().is_empty() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let left = marker.alive();
        assert!(left.is_empty(), "{} forked children survived", left.len());
        assert!(sweep.complete);
        assert!(alive(&mut root));
    }
}
