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
pub(crate) mod test_fixtures;
#[cfg(test)]
pub(crate) use test_fixtures::signal as fixture_signal;

#[cfg(test)]
mod tests {
    use super::test_fixtures::forked_tree::{fork_tree, fork_tree_with, ForkedTree};
    use super::test_fixtures::marked_sleeps::{descends_from_with, MarkedSleeps};
    use super::test_fixtures::me;
    use super::test_fixtures::reaped::Reaped;
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    fn sleeper() -> Reaped {
        Reaped(Command::new("/usr/bin/sleep").arg("30").spawn().unwrap())
    }
    fn alive(child: &mut Reaped) -> bool {
        child.0.try_wait().unwrap().is_none()
    }
    fn wait_dead(child: &mut Reaped) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if !alive(child) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
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
        while !dead(descendant) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            dead(descendant),
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

    /// `C` is P's child; a recorded `child` that is not (here a bystander
    /// sleeper standing for a recycled pid) is not signalled by the drop.
    #[test]
    fn a_fork_tree_drop_spares_a_child_that_is_not_its_roots() {
        let mut bystander = marked_sleep(&unique_marker());
        let tree = fork_tree(nothing);
        let root = tree.root;
        std::mem::forget(tree); // P is killed and reaped below; C dies with it
        drop(ForkedTree {
            root,
            child: bystander.0.id() as i32,
        });
        assert!(
            survives_a_settle(&mut bystander),
            "the drop killed a process that is not its root's child"
        );
    }

    fn nothing() {}

    #[test]
    fn a_failed_first_fork_is_an_error() {
        assert!(fork_tree_with(nothing, || -1).is_err());
    }

    #[test]
    fn a_failed_second_fork_is_an_error_and_signals_nothing_unsafe() {
        // The first call (here) really forks; the root's copy of the counter
        // says 1, so its own fork "fails".
        let calls = std::sync::atomic::AtomicU32::new(0);
        let tree = fork_tree_with(nothing, || {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                -1
            } else {
                unsafe { libc::fork() }
            }
        });
        // Reaching here at all means -1 was never used as a kill target.
        assert!(tree.is_err());
    }

    /// The harness dying (SIGKILL, so no destructor runs) must not leave the
    /// paused `P` and `C` behind. Re-execs the test binary as a harness that
    /// builds a tree and is then killed (only that child, by its handle).
    #[test]
    fn a_killed_harness_leaves_no_fork_tree_behind() {
        const CHILD: &str = "SNITCHWATCH_TEST_ORPHANED_TREE";
        if let Some(file) = std::env::var_os(CHILD) {
            let tree = fork_tree(nothing);
            std::fs::write(&file, format!("{} {}\n", tree.root, tree.child)).unwrap();
            loop {
                unsafe { libc::pause() };
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pids");
        let marker = unique_marker();
        // The extra argument is a filter matching no test; it puts the marker
        // in the argv that `P` and `C` (forked, never exec'd) carry.
        let mut harness = Reaped(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "containment::tests::a_killed_harness_leaves_no_fork_tree_behind",
                    "--test-threads=1",
                    &marker,
                ])
                .env(CHILD, &file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let reported = || -> Vec<i32> {
            match std::fs::read_to_string(&file) {
                Ok(text) if text.ends_with('\n') => text
                    .split_whitespace()
                    .filter_map(|p| p.parse().ok())
                    .collect(),
                _ => Vec::new(),
            }
        };
        let start = Instant::now();
        while reported().len() < 2 && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids = reported();
        assert_eq!(pids.len(), 2, "the harness never built its tree");
        harness.0.kill().unwrap();
        harness.0.wait().unwrap();
        let start = Instant::now();
        while !pids.iter().all(|&p| dead(p)) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let orphans: Vec<i32> = pids.iter().copied().filter(|&p| !dead(p)).collect();
        for &pid in &orphans {
            fixture_signal::kill_marked(pid, &marker);
        }
        assert!(orphans.is_empty(), "paused orphans: {orphans:?}");
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
        // A vanished entry is dead; any other read error is not an answer.
        let alive = match std::fs::read(format!("/proc/{child}/stat")) {
            Ok(stat) => {
                let tail = &stat[stat.iter().rposition(|&b| b == b')').unwrap() + 1..];
                tail.trim_ascii_start().first() != Some(&b'Z')
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => panic!("cannot read the descendant's stat: {e}"),
        };
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
        let marker = unique_marker();
        let mut fixture = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "containment::tests::an_exhausted_fd_table_fails_the_scan_instead_of_emptying_it",
                "--test-threads=1",
                marker.as_str(),
            ])
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        // Polled without reaping, so the group kill below cannot name a
        // recycled pid; then the group first (the sleeper is in it), then reap.
        let finished = exited_unreaped(&fixture, Duration::from_secs(20));
        fixture_signal::killpg_marked(fixture.id() as i32, &marker);
        let status = fixture.wait().unwrap();
        assert!(finished, "the fixture never finished");
        assert_eq!(
            status.code(),
            Some(0),
            "3: the scan came back Ok; 5: complete; 6: no failure counted ({status:?})"
        );
    }

    fn unique_marker() -> String {
        fixture_signal::unique_marker()
    }

    /// Whether `child` has exited, polled until `bound` without reaping it:
    /// its pid and group stay reserved for a later group kill.
    fn exited_unreaped(child: &Child, bound: Duration) -> bool {
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

    #[test]
    fn cleanup_kills_recorded_sleepers_even_when_every_scan_fails() {
        let root = own_root();
        let marker = unique_marker();
        let spawn = || {
            Reaped(
                Command::new("/usr/bin/sleep")
                    .arg0("sleep")
                    .arg(&marker)
                    .spawn()
                    .unwrap(),
            )
        };
        let (mut a, mut b) = (spawn(), spawn());
        let fixture = MarkedSleeps::with_scan(marker.clone(), root.0.id() as i32, |_, _| {
            Err(io::Error::from_raw_os_error(libc::EMFILE))
        });
        assert!(fixture
            .try_alive(Instant::now() + Duration::from_secs(1))
            .is_err());
        let (pa, pb) = (a.0.id() as i32, b.0.id() as i32);
        fixture.record(&[pa, pb]);
        let kills = fixture.cleanup();
        drop(fixture);
        // Both are waited for (no short circuit); on failure say what each
        // looked like and what a second verified kill does, to tell a missed
        // identity check from a kill that was sent but not yet effective.
        let dead = [wait_dead(&mut a), wait_dead(&mut b)];
        if !dead.iter().all(|&d| d) {
            let report: Vec<String> = [(pa, dead[0]), (pb, dead[1])]
                .iter()
                .filter(|(_, d)| !d)
                .map(|&(pid, _)| {
                    format!(
                        "pid {pid}: cmdline {:?}, stat {:?}, retried kill {:?}",
                        std::fs::read(format!("/proc/{pid}/cmdline"))
                            .map(|c| String::from_utf8_lossy(&c).into_owned()),
                        parent_and_zombie(pid),
                        fixture_signal::kill_marked(pid, &marker),
                    )
                })
                .collect();
            panic!("recorded sleepers survived: {report:?}; cleanup kills: {kills:?}");
        }
    }

    use fixture_signal::{dead, dead_with};

    #[test]
    fn an_unreadable_proc_entry_is_an_error_not_dead() {
        let emfile = |_| Err(io::Error::from_raw_os_error(libc::EMFILE));
        assert!(dead_with(4242, emfile).is_err());
        assert!(dead_with(4242, |_| Ok(None)).unwrap());
        assert!(dead_with(4242, |_| Ok(Some((1, true)))).unwrap());
        assert!(!dead_with(4242, |_| Ok(Some((1, false)))).unwrap());
    }

    /// Pids here are numbers in fake tables only; nothing is signalled.
    #[test]
    fn a_parent_recycled_between_the_read_and_the_pin_forges_no_ancestry() {
        use std::cell::Cell;
        let root = 7;
        let reads_of_target = Cell::new(0);
        let open = |_: i32| std::fs::File::open("/dev/null").map(Into::into);
        // Target 100 had parent 50 (first read); 50 then exited and its number
        // was reused by a child of the root, so by the re-read 100 is
        // orphaned (parent 1) while 50 now reaches the root.
        let stat = |p: i32| -> io::Result<Option<(i32, bool)>> {
            Ok(match p {
                100 => {
                    reads_of_target.set(reads_of_target.get() + 1);
                    Some((if reads_of_target.get() == 1 { 50 } else { 1 }, false))
                }
                50 => Some((root, false)),
                _ => None,
            })
        };
        assert!(!descends_from_with(100, root, open, stat));
    }

    #[test]
    fn a_pinned_stable_chain_reaches_the_root_and_a_failed_pin_spares() {
        let root = 7;
        let table = |p: i32| -> io::Result<Option<(i32, bool)>> {
            Ok(match p {
                100 => Some((50, false)),
                50 => Some((root, false)),
                _ => None,
            })
        };
        let opened = std::cell::RefCell::new(Vec::new());
        let open = |p: i32| {
            opened.borrow_mut().push(p);
            std::fs::File::open("/dev/null").map(Into::into)
        };
        assert!(descends_from_with(100, root, open, table));
        assert_eq!(*opened.borrow(), [50]);
        let refuse = |_| Err(io::Error::from_raw_os_error(libc::EMFILE));
        assert!(!descends_from_with(100, root, refuse, table));
    }

    /// Whether `child` is still running after a signal sent to it would have
    /// had time to land (a SIGKILL is not delivered synchronously).
    fn survives_a_settle(child: &mut Reaped) -> bool {
        std::thread::sleep(Duration::from_millis(300));
        alive(child)
    }

    fn failing_scan(_: i32, _: Instant) -> io::Result<Option<Vec<i32>>> {
        Err(io::Error::from_raw_os_error(libc::EMFILE))
    }

    fn marked_sleep(marker: &str) -> Reaped {
        Reaped(
            Command::new("/usr/bin/sleep")
                .arg0("sleep")
                .arg(marker)
                .spawn()
                .unwrap(),
        )
    }

    fn devnull_fd(_: i32) -> io::Result<std::os::fd::OwnedFd> {
        Ok(std::os::fd::OwnedFd::from(
            std::fs::File::open("/dev/null").unwrap(),
        ))
    }

    #[test]
    fn killpg_verified_branches() {
        use fixture_signal::{killpg_verified_with, Verified};
        use std::cell::Cell;
        let sent = Cell::new(0);
        let go = |g: i32| sent.set(g);
        let eperm = |_| Err(io::Error::from_raw_os_error(libc::EPERM));
        let esrch = |_| Err(io::Error::from_raw_os_error(libc::ESRCH));
        let unreachable_open = |_| -> io::Result<std::os::fd::OwnedFd> { panic!("opened") };
        for bad in [-1, 0, 1] {
            assert_eq!(
                killpg_verified_with(bad, unreachable_open, go, |_| true),
                Verified::Unsafe
            );
        }
        assert_eq!(
            killpg_verified_with(4242, eperm, go, |_| true),
            Verified::Refused(libc::EPERM)
        );
        assert_eq!(
            killpg_verified_with(4242, esrch, go, |_| true),
            Verified::Gone
        );
        assert_eq!(
            killpg_verified_with(4242, devnull_fd, go, |_| false),
            Verified::Mismatch
        );
        assert_eq!(sent.get(), 0, "signalled without a verified leader");
        assert_eq!(
            killpg_verified_with(4242, devnull_fd, go, |_| true),
            Verified::Sent
        );
        assert_eq!(sent.get(), 4242);
    }

    /// A pid that is not a child of ours (here a sleeper orphaned from its
    /// shell, standing for a recycled pid), with a marker elsewhere.
    fn orphan_sleeper(marker: &str) -> i32 {
        let out = Command::new("/usr/bin/sh")
            .args([
                "-c",
                &format!("sleep {marker} >/dev/null 2>&1 </dev/null & echo $!"),
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    /// A group leader that is neither marked, ours, nor a zombie child of
    /// ours keeps its group.
    #[test]
    fn killpg_marked_spares_a_leader_it_cannot_vouch_for() {
        let marker = unique_marker();
        let pid = orphan_sleeper(&marker);
        let verdict = fixture_signal::killpg_marked(pid, "some-other-marker");
        std::thread::sleep(Duration::from_millis(200));
        let survived = !dead(pid);
        fixture_signal::kill_marked(pid, &marker);
        assert_eq!(verdict, fixture_signal::Verified::Mismatch);
        assert!(
            survived,
            "the group of an unidentified leader was signalled"
        );
    }

    /// Our own unreaped child leads its group: marker or not, live or
    /// zombie, the group (leader included) is killed.
    #[test]
    fn killpg_marked_kills_a_group_whose_leader_is_ours() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        let marker = unique_marker();
        let mut live = Command::new("/usr/bin/sleep")
            .arg(&marker)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut zombie = Command::new("/usr/bin/true")
            .process_group(0)
            .spawn()
            .unwrap();
        let exited = exited_unreaped(&zombie, Duration::from_secs(5));
        let a = fixture_signal::killpg_marked(live.id() as i32, "some-other-marker");
        let b = fixture_signal::killpg_marked(zombie.id() as i32, "some-other-marker");
        let live_status = live.wait().unwrap();
        zombie.wait().unwrap();
        assert!(exited);
        assert_eq!(a, fixture_signal::Verified::Sent);
        assert_eq!(b, fixture_signal::Verified::Sent);
        assert_eq!(live_status.signal(), Some(libc::SIGKILL));
    }

    /// The root of a fork tree that is not our child (a recycled pid) is not
    /// signalled by the drop.
    #[test]
    fn a_fork_tree_drop_spares_a_root_that_is_not_ours() {
        let marker = unique_marker();
        let pid = orphan_sleeper(&marker);
        drop(ForkedTree {
            root: pid,
            child: 0,
        });
        std::thread::sleep(Duration::from_millis(200));
        let survived = !dead(pid);
        fixture_signal::kill_marked(pid, &marker);
        assert!(survived, "the drop killed a root that is not our child");
    }

    #[test]
    fn with_scan_refuses_the_test_process_as_root() {
        let refused = std::panic::catch_unwind(|| {
            MarkedSleeps::with_scan(unique_marker(), me(), failing_scan)
        });
        assert!(refused.is_err(), "accepted the test process as the root");
    }

    /// Everything an escaped `setsid` shell started is killed even though no
    /// scan ever succeeded: the script reported its own pids.
    #[test]
    fn drop_kills_the_pid_file_processes_when_every_scan_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pids");
        let marker = unique_marker();
        let script = format!(
            "setsid sh -c 'echo $$ >> {f}; sleep {m} & echo $! >> {f}; sleep {m} & echo $! >> {f}; wait' & read _",
            f = file.display(),
            m = marker
        );
        let root = Reaped(
            Command::new("/usr/bin/sh")
                .args(["-c", &script])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let fixture = MarkedSleeps::with_scan(marker.clone(), root.0.id() as i32, failing_scan)
            .with_pid_file(file.clone());
        let read = || -> Vec<i32> {
            std::fs::read_to_string(&file)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.parse().ok())
                .collect()
        };
        let start = Instant::now();
        while read().len() < 3 && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let pids = read();
        assert_eq!(pids.len(), 3, "the fixture never reported its pids");
        drop(fixture);
        let start = Instant::now();
        while !pids.iter().all(|&p| dead(p)) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors: Vec<i32> = pids.iter().copied().filter(|&p| !dead(p)).collect();
        for &pid in &survivors {
            fixture_signal::kill_marked(pid, &marker);
        }
        assert!(survivors.is_empty(), "survived the cleanup: {survivors:?}");
    }

    static DECOY: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    fn scan_returns_the_decoy(_: i32, _: Instant) -> io::Result<Option<Vec<i32>>> {
        Ok(Some(vec![DECOY.load(std::sync::atomic::Ordering::SeqCst)]))
    }

    /// Phase 2 signals only a process whose parent is the root or another
    /// scanned process; a scan result naming anything else is left alone.
    #[test]
    fn drop_does_not_kill_a_scanned_non_descendant() {
        let root = own_root();
        let mut decoy = sleeper();
        DECOY.store(decoy.0.id() as i32, std::sync::atomic::Ordering::SeqCst);
        drop(MarkedSleeps::with_scan(
            unique_marker(),
            root.0.id() as i32,
            scan_returns_the_decoy,
        ));
        let survived = survives_a_settle(&mut decoy);
        DECOY.store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(survived, "phase 2 killed a non-descendant");
    }

    static STALE: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());
    fn scan_returns_the_stale_list(_: i32, _: Instant) -> io::Result<Option<Vec<i32>>> {
        Ok(Some(STALE.lock().unwrap().clone()))
    }

    /// Phase 2 trusts neither the stale scan's parent pids nor their reuse: a
    /// process outside the fixture whose parent is a scanned pid (here a
    /// bystander shell, with its own sleeper as the "recycled child") has
    /// neither the marker nor a live chain to the root, so it is not killed.
    #[test]
    fn drop_does_not_kill_a_child_of_a_scanned_but_unrelated_pid() {
        use std::io::Read;
        let root = own_root();
        let mut holder = Reaped(
            Command::new("/usr/bin/sh")
                .args(["-c", &format!("sleep {} & echo $!; wait", unique_marker())])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = [0u8; 32];
        let n = holder.0.stdout.as_mut().unwrap().read(&mut line).unwrap();
        let decoy: i32 = String::from_utf8_lossy(&line[..n]).trim().parse().unwrap();
        let holder_pid = holder.0.id() as i32;
        *STALE.lock().unwrap() = vec![holder_pid, decoy];
        drop(MarkedSleeps::with_scan(
            unique_marker(),
            root.0.id() as i32,
            scan_returns_the_stale_list,
        ));
        STALE.lock().unwrap().clear();
        std::thread::sleep(Duration::from_millis(300));
        let spared = !dead(decoy);
        // The decoy is not our direct child: end it through the holder.
        fixture_signal::kill_verified(decoy, |p| {
            parent_and_zombie(p).is_some_and(|(parent, _)| parent == holder_pid)
        });
        assert!(spared, "phase 2 killed a child of an unrelated scanned pid");
    }

    /// Phase 1: a recorded pid that no longer carries the marker (reused) is
    /// not signalled.
    #[test]
    fn drop_does_not_kill_a_recycled_recorded_pid() {
        let root = own_root();
        let mut other = marked_sleep(&unique_marker());
        let fixture = MarkedSleeps::with_scan(unique_marker(), root.0.id() as i32, failing_scan);
        fixture.record(&[other.0.id() as i32]);
        drop(fixture);
        assert!(
            survives_a_settle(&mut other),
            "killed a pid without the marker"
        );
    }

    /// A fixture root of its own: a child that only waits. Cleanup of a
    /// fixture rooted here can touch nothing but this child's subtree.
    fn own_root() -> Reaped {
        Reaped(
            Command::new("/usr/bin/sh")
                .args(["-c", "read _"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }

    #[test]
    fn a_failed_observation_is_an_error_not_an_empty_list() {
        let root = own_root();
        let fixture = MarkedSleeps::new(unique_marker(), root.0.id() as i32);
        let past = Instant::now() - Duration::from_secs(1);
        assert!(fixture.try_alive(past).is_err());
    }

    /// A fixture whose assertion fails unwinds through its cleanup, which
    /// must sweep its own subtree only: a sibling owned by another test
    /// (here a plain child of the test process) must survive.
    #[test]
    fn a_failing_fixture_does_not_clean_up_a_sibling() {
        let mut sibling = sleeper();
        let root = own_root();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let fixture = MarkedSleeps::new(unique_marker(), root.0.id() as i32);
            fixture
                .try_alive(Instant::now() - Duration::from_secs(1))
                .unwrap();
        }));
        assert!(unwound.is_err());
        assert!(
            alive(&mut sibling),
            "the unwinding fixture killed a sibling"
        );
        drop(root);
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
        let marker = MarkedSleeps::new("29.7316".into(), fixture_root.0.id() as i32);
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
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pids");
        let mut command = Command::new("/usr/bin/sh");
        // Like the scanner, `root` adopts orphans: they stay in its subtree.
        unsafe {
            command.pre_exec(become_subreaper);
        }
        let mut root = Reaped(
            command
                .args([
                    "-c",
                    &format!(
                        "setsid sh -c 'echo $$ >> {f}; n=0; while [ $n -lt 300 ]; do sleep {name} & echo $! >> {f}; n=$((n+1)); done; wait' & read _",
                        f = file.display()
                    ),
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let marker = MarkedSleeps::new(name, root.0.id() as i32).with_pid_file(file);
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
