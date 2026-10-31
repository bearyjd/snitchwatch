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

/// SIGKILL `pid` only if, once pinned by a pidfd, it is still a child of one
/// of `tree` (the scanner or a process found below it). A pid reused by an
/// unrelated process before the pidfd opened fails that check; one reused
/// after it can't be signalled through the pidfd, which names the original.
fn kill_if_descendant(pid: i32, tree: &HashSet<i32>) {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        // Fail safe on a kernel without pidfds (before 5.3): kill by pid after
        // the same parent check, accepting the reuse window pidfds close.
        if io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS)
            && parent_and_zombie(pid).is_some_and(|(parent, _)| tree.contains(&parent))
        {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        return;
    }
    let fd = fd as i32;
    if parent_and_zombie(pid).is_some_and(|(parent, _)| tree.contains(&parent)) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            );
        }
    }
    unsafe {
        libc::close(fd);
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
            kill_if_descendant(pid, &tree);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
