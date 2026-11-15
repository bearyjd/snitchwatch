//! The sweep's view of the scanner's subtree: walk `/proc/<pid>/task/<tid>/children`
//! from the root instead of reading every `/proc` entry, and let one
//! unreadable pid mark the pass incomplete instead of discarding it.
//!
//! The kernel lists a child under the *thread* that forked it, so every task
//! of every visited process is read. A child forked after its parent's list
//! was read is caught by the next pass; the sweep already needs two
//! consecutive empty complete passes before it calls the subtree empty.
//! Without `CONFIG_PROC_CHILDREN` (no `children` file) the caller falls back
//! to a full `/proc` scan.

use super::{vanished, Stat};
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

/// One scan of the subtree. `complete == false` means something could not be
/// read (`first_error`) or the deadline hit: `live` is still everything that
/// was found, but the pass is no evidence that nothing else is alive.
#[derive(Debug)]
pub(super) struct Pass {
    pub live: Vec<(i32, u64)>,
    pub complete: bool,
    pub first_error: Option<io::Error>,
}

impl Pass {
    fn new() -> Self {
        Pass {
            live: Vec::new(),
            complete: true,
            first_error: None,
        }
    }

    fn fail(&mut self, e: io::Error) {
        self.complete = false;
        self.first_error.get_or_insert(e);
    }
}

/// The children of one pid, over all its tasks. `error`: some task's list
/// could not be read (the others are still in `pids`); `unsupported`: the
/// kernel has no `children` file.
#[derive(Debug, Default)]
pub(super) struct Kids {
    pub pids: Vec<i32>,
    pub error: Option<io::Error>,
    pub unsupported: bool,
    /// The deadline hit before every task was read.
    pub truncated: bool,
}

/// The per-pid `/proc` reads, injectable so tests need no real EMFILE.
pub(super) trait ProcView {
    /// `Ok(None)`: the process vanished.
    fn stat(&self, pid: i32) -> io::Result<Option<Stat>>;
    fn has_other_tasks(&self, pid: i32) -> io::Result<bool>;
    fn children(&self, pid: i32, deadline: Instant) -> Kids;
    /// `Ok(None)`: the deadline hit first.
    fn all_pids(&self, deadline: Instant) -> io::Result<Option<Vec<i32>>>;
}

/// The real `/proc`.
pub(super) struct Procfs;

impl ProcView for Procfs {
    fn stat(&self, pid: i32) -> io::Result<Option<Stat>> {
        super::read_stat_with_start(pid)
    }

    fn has_other_tasks(&self, pid: i32) -> io::Result<bool> {
        super::has_other_tasks(pid)
    }

    fn children(&self, pid: i32, _deadline: Instant) -> Kids {
        let mut kids = Kids::default();
        let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
            Ok(tasks) => tasks,
            Err(e) if vanished(&e) => return kids,
            Err(e) => {
                kids.error = Some(e);
                return kids;
            }
        };
        for task in tasks {
            match task {
                Ok(task) => {
                    let tid = task
                        .file_name()
                        .to_str()
                        .and_then(|t| t.parse::<i32>().ok());
                    if let Some(tid) = tid {
                        read_task_children(pid, tid, &mut kids);
                    }
                }
                Err(e) if vanished(&e) => {}
                Err(e) => {
                    kids.error.get_or_insert(e);
                }
            }
        }
        kids
    }

    fn all_pids(&self, _deadline: Instant) -> io::Result<Option<Vec<i32>>> {
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            match entry {
                Ok(entry) => pids.extend(
                    entry
                        .file_name()
                        .to_str()
                        .and_then(|n| n.parse::<i32>().ok()),
                ),
                Err(e) if vanished(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Some(pids))
    }
}

/// Append the pids in `/proc/<pid>/task/<tid>/children` to `kids`. A missing
/// file under a task that still exists means the kernel has no such file; under
/// a task that is gone it means the thread exited.
fn read_task_children(pid: i32, tid: i32, kids: &mut Kids) {
    match std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/children")) {
        Ok(text) => {
            for word in text.split_ascii_whitespace() {
                match word.parse() {
                    Ok(child) => kids.pids.push(child),
                    Err(_) => {
                        kids.error.get_or_insert_with(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "malformed children list")
                        });
                    }
                }
            }
        }
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            if std::path::Path::new(&format!("/proc/{pid}/task/{tid}")).exists() {
                kids.unsupported = true;
            }
        }
        Err(e) if vanished(&e) => {}
        Err(e) => {
            kids.error.get_or_insert(e);
        }
    }
}

/// Whether a process is dead: a zombie with no other thread left.
fn is_dead(view: &impl ProcView, pid: i32, stat: &Stat) -> io::Result<bool> {
    Ok(stat.zombie && !view.has_other_tasks(pid)?)
}

/// Walk the subtree below `root`. `None`: this kernel has no `children`
/// files, use [`scan_all`].
pub(super) fn walk(root: i32, deadline: Instant, view: &impl ProcView) -> Option<Pass> {
    let mut pass = Pass::new();
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        if Instant::now() >= deadline {
            pass.complete = false;
            break;
        }
        let kids = view.children(parent, deadline);
        if kids.unsupported {
            if parent == root {
                return None;
            }
            pass.fail(io::Error::other("no /proc children file below the root"));
        }
        if let Some(e) = kids.error {
            pass.fail(e);
        }
        for pid in kids.pids {
            if seen.insert(pid) {
                note_live(view, pid, &mut pass);
                // Walk through dead nodes too: a list can name a child under
                // a parent that is already exiting.
                stack.push(pid);
            }
        }
    }
    Some(pass)
}

/// Record `pid` as live if it is; an unreadable one only fails the pass.
fn note_live(view: &impl ProcView, pid: i32, pass: &mut Pass) {
    match view.stat(pid).and_then(|stat| {
        stat.map(|stat| is_dead(view, pid, &stat).map(|dead| (stat, dead)))
            .transpose()
    }) {
        Ok(Some((stat, false))) => pass.live.push((pid, stat.start)),
        Ok(_) => {}
        Err(e) => pass.fail(e),
    }
}

/// The fallback: read every `/proc` entry and follow parent links from `root`.
/// A pid whose own stat is unreadable hides what hangs below it (its parent
/// link is unknown); the pass is marked incomplete.
pub(super) fn scan_all(root: i32, deadline: Instant, view: &impl ProcView) -> io::Result<Pass> {
    let mut pass = Pass::new();
    let mut children: HashMap<i32, Vec<(i32, bool, u64)>> = HashMap::new();
    let Some(all) = view.all_pids(deadline)? else {
        pass.complete = false;
        return Ok(pass);
    };
    for pid in all {
        // A pass over a huge /proc must not outlast the bound.
        if Instant::now() >= deadline {
            pass.complete = false;
            return Ok(pass);
        }
        match view.stat(pid).and_then(|stat| {
            stat.map(|stat| is_dead(view, pid, &stat).map(|dead| (stat, dead)))
                .transpose()
        }) {
            Ok(Some((stat, dead))) => children
                .entry(stat.parent)
                .or_default()
                .push((pid, dead, stat.start)),
            Ok(None) => {}
            Err(e) => pass.fail(e),
        }
    }
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(parent) = stack.pop() {
        for &(pid, dead, start) in children.get(&parent).into_iter().flatten() {
            if seen.insert(pid) {
                if !dead {
                    pass.live.push((pid, start));
                }
                stack.push(pid);
            }
        }
    }
    Ok(pass)
}

/// [`walk`], else [`scan_all`].
pub(super) fn live_members_via(
    root: i32,
    deadline: Instant,
    view: &impl ProcView,
) -> io::Result<Pass> {
    match walk(root, deadline, view) {
        Some(pass) => Ok(pass),
        None => scan_all(root, deadline, view),
    }
}

#[cfg(test)]
mod tests;
