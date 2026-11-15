//! The sweep's view of the scanner's subtree: walk `/proc/<pid>/task/<tid>/children`
//! from the root instead of reading every `/proc` entry, and let one
//! unreadable pid mark the pass incomplete instead of discarding it.
//!
//! The kernel lists a child under the *thread* that forked it, so every task
//! of every visited process is read. A child forked after its parent's list
//! was read is caught by the next pass; the sweep already needs two
//! consecutive empty complete passes before it calls the subtree empty.
//! The scanner is a subreaper, so a node that exits hands its children to the
//! root: the walk re-reads the root's list until it stops changing (bounded
//! by a round cap and the deadline, else the pass is incomplete), so a
//! fork-relay cannot hide an orphan from consecutive passes.
//!
//! Parenthood comes from the child's own stat, not from its parent's list,
//! and a node's start time is re-checked after its list is read: a pid reused
//! by an unrelated process is never admitted, so nothing below it can be
//! signalled. A pid whose stat cannot be read is not walked below (it cannot
//! be vouched for); the pass is incomplete instead. A listed child that has
//! vanished may have forked first, so it also makes the pass incomplete.
//!
//! Accepted limits, not coded around: (1) the fallback full scan (kernels
//! without `CONFIG_PROC_CHILDREN`) reads `/proc` non-atomically, so a fork
//! relay can hide from two passes there; the walk is the default, and the
//! fallback is unreachable on Fedora/Bazzite kernels (`CONFIG_PROC_CHILDREN=y`).
//! (2) The kernel documents `children` as exact only while the tasks are
//! stopped; a miss is for the next pass to catch.
//! Without `CONFIG_PROC_CHILDREN` (no `children` file) the caller falls back
//! to a full `/proc` scan.

use super::{vanished, Stat};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
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

    fn children(&self, pid: i32, deadline: Instant) -> Kids {
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
            if Instant::now() >= deadline {
                kids.truncated = true;
                break;
            }
            match task {
                Ok(task) => {
                    let tid = task
                        .file_name()
                        .to_str()
                        .and_then(|t| t.parse::<i32>().ok());
                    if let Some(tid) = tid {
                        read_task_children(pid, tid, deadline, &mut kids);
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

    fn all_pids(&self, deadline: Instant) -> io::Result<Option<Vec<i32>>> {
        let mut pids = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            if Instant::now() >= deadline {
                return Ok(None);
            }
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

/// The most bytes read from one `children` file.
const MAX_CHILDREN_BYTES: u64 = 1 << 20;

/// Parse a `children` file's pids into `kids`: at most [`MAX_CHILDREN_BYTES`]
/// are read and the deadline is checked every 256 pids. A longer file is
/// marked truncated but its readable prefix is still parsed, cut at the last
/// whitespace so a half-read pid is never admitted (each pid is stat-verified
/// later anyway).
fn parse_children(src: impl std::io::Read, deadline: Instant, kids: &mut Kids) {
    let mut bytes = Vec::new();
    match src.take(MAX_CHILDREN_BYTES + 1).read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(e) if vanished(&e) => return,
        Err(e) => {
            kids.error.get_or_insert(e);
            return;
        }
    }
    if bytes.len() as u64 > MAX_CHILDREN_BYTES {
        kids.truncated = true;
        // The extra byte only proves there was more; keep whole tokens.
        let extra_is_space = bytes.last().is_some_and(u8::is_ascii_whitespace);
        bytes.pop();
        if !extra_is_space {
            let keep = bytes
                .iter()
                .rposition(u8::is_ascii_whitespace)
                .map_or(0, |i| i + 1);
            bytes.truncate(keep);
        }
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        kids.error.get_or_insert_with(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed children list")
        });
        return;
    };
    for (i, word) in text.split_ascii_whitespace().enumerate() {
        if i % 256 == 0 && Instant::now() >= deadline {
            kids.truncated = true;
            return;
        }
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

/// Append the pids in `/proc/<pid>/task/<tid>/children` to `kids`. A missing
/// file under a task that still exists means the kernel has no such file; under
/// a task that is gone it means the thread exited.
fn read_task_children(pid: i32, tid: i32, deadline: Instant, kids: &mut Kids) {
    match std::fs::File::open(format!("/proc/{pid}/task/{tid}/children")) {
        Ok(file) => parse_children(file, deadline, kids),
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

/// Whether a process is dead: a zombie with no other thread left. `None`: it
/// vanished while being checked.
///
/// Death is re-read after the task listing: a surviving thread of a zombie
/// leader can `execve`, take over the leader's pid (and start time) and make
/// it a live process, so the first stat alone may be stale. Anything but the
/// same zombie on the second read counts as live.
fn is_dead(view: &impl ProcView, pid: i32, stat: &Stat) -> io::Result<Option<bool>> {
    if !stat.zombie || view.has_other_tasks(pid)? {
        return Ok(Some(false));
    }
    Ok(view
        .stat(pid)?
        .map(|again| again.zombie && again.parent == stat.parent && again.start == stat.start))
}

/// Rounds of re-reading the root's list after the walk before the pass gives
/// up and is marked incomplete.
const MAX_ROUNDS: usize = 4;

/// Walk the subtree below `root`. `None`: this kernel has no `children`
/// files, use [`scan_all`].
///
/// Parenthood is checked against each child's own stat (a listed pid may have
/// been reused by an unrelated process), and a node's identity (start time)
/// is re-checked after its list is read. Because the scanner is a subreaper,
/// an exiting node's children reparent onto `root`: after the walk the root's
/// list is read again and anything new is walked, until a re-read adds nothing
/// or [`MAX_ROUNDS`] is hit (incomplete). Every doubt marks the pass incomplete.
pub(super) fn walk(root: i32, deadline: Instant, view: &impl ProcView) -> Option<Pass> {
    let mut w = Walk {
        view,
        deadline,
        pass: Pass::new(),
        seen: HashSet::from([root]),
        stack: Vec::new(),
    };
    if Instant::now() >= deadline {
        w.pass.complete = false;
        return Some(w.pass);
    }
    let root_start = match view.stat(root) {
        Ok(Some(stat)) => stat.start,
        Ok(None) => {
            w.pass.complete = false;
            return Some(w.pass);
        }
        Err(e) => {
            w.pass.fail(e);
            return Some(w.pass);
        }
    };
    if w.expand(root, root_start) {
        return None;
    }
    let mut rounds = 0;
    loop {
        while let Some((node, start)) = w.stack.pop() {
            if w.expand(node, start) {
                w.pass
                    .fail(io::Error::other("no /proc children file below the root"));
            }
        }
        if rounds == MAX_ROUNDS {
            w.pass.complete = false;
            break;
        }
        rounds += 1;
        if w.expand(root, root_start) {
            w.pass
                .fail(io::Error::other("root children file vanished on re-read"));
        }
        if w.stack.is_empty() {
            break;
        }
    }
    Some(w.pass)
}

struct Walk<'a, V: ProcView> {
    view: &'a V,
    deadline: Instant,
    pass: Pass,
    seen: HashSet<i32>,
    /// Admitted nodes whose lists are still to be read, with their start times.
    stack: Vec<(i32, u64)>,
}

impl<V: ProcView> Walk<'_, V> {
    fn out_of_time(&mut self) -> bool {
        let out = Instant::now() >= self.deadline;
        if out {
            self.pass.complete = false;
        }
        out
    }

    /// Read `node`'s children and admit the new ones. `true`: the kernel has
    /// no `children` file. If `node` is no longer the process admitted (its
    /// start time changed or it is gone) nothing it listed is trusted.
    fn expand(&mut self, node: i32, start: u64) -> bool {
        if self.out_of_time() {
            return false;
        }
        let kids = self.view.children(node, self.deadline);
        if kids.unsupported {
            return true;
        }
        if kids.truncated {
            self.pass.complete = false;
        }
        if let Some(e) = kids.error {
            self.pass.fail(e);
        }
        match self.view.stat(node) {
            Ok(Some(now)) if now.start == start => {}
            Ok(_) => {
                self.pass.complete = false;
                return false;
            }
            Err(e) => {
                self.pass.fail(e);
                return false;
            }
        }
        let mut batch: Vec<Admitted> = Vec::new();
        let mut in_batch: HashSet<i32> = HashSet::new();
        for pid in kids.pids {
            if self.out_of_time() {
                break;
            }
            if !self.seen.contains(&pid) && in_batch.insert(pid) {
                batch.extend(self.admit(pid, node));
            }
        }
        // The pids above were only trusted while `node` stayed the same
        // process (it and a child could both exit and be reused meanwhile):
        // commit them only if it did.
        match self.view.stat(node) {
            Ok(Some(now)) if now.start == start => self.commit(batch),
            Ok(_) => self.pass.complete = false,
            Err(e) => self.pass.fail(e),
        }
        false
    }

    /// Record a verified batch: seen, live and (unless dead) still to expand.
    fn commit(&mut self, batch: Vec<Admitted>) {
        for admitted in batch {
            self.seen.insert(admitted.pid);
            if admitted.dead {
                // No children left to list; expanding it would only fail
                // spuriously once it is reaped.
                continue;
            }
            self.pass.live.push((admitted.pid, admitted.start));
            self.stack.push((admitted.pid, admitted.start));
        }
    }

    /// `pid`, listed as a child of `parent`, if its own stat says so. A
    /// vanished or doubtful one is skipped and the pass marked incomplete
    /// (a vanished child may have forked first).
    fn admit(&mut self, pid: i32, parent: i32) -> Option<Admitted> {
        let stat = match self.view.stat(pid) {
            Ok(Some(stat)) => stat,
            Ok(None) => {
                self.pass.complete = false;
                return None;
            }
            Err(e) => {
                self.pass.fail(e);
                return None;
            }
        };
        if stat.parent != parent {
            self.pass.complete = false;
            return None;
        }
        match is_dead(self.view, pid, &stat) {
            Ok(Some(dead)) => Some(Admitted {
                pid,
                start: stat.start,
                dead,
            }),
            Ok(None) => {
                self.pass.complete = false;
                None
            }
            Err(e) => {
                self.pass.fail(e);
                None
            }
        }
    }
}

/// A listed child that passed its own checks, not yet committed.
struct Admitted {
    pid: i32,
    start: u64,
    dead: bool,
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
            stat.map(|stat| is_dead(view, pid, &stat).map(|dead| dead.map(|dead| (stat, dead))))
                .transpose()
                .map(Option::flatten)
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
        if Instant::now() >= deadline {
            pass.complete = false;
            return Ok(pass);
        }
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
