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
}

/// The per-pid `/proc` reads, injectable so tests need no real EMFILE.
pub(super) trait ProcView {
    /// `Ok(None)`: the process vanished.
    fn stat(&self, pid: i32) -> io::Result<Option<Stat>>;
    fn has_other_tasks(&self, pid: i32) -> io::Result<bool>;
    fn children(&self, pid: i32) -> Kids;
    fn all_pids(&self) -> io::Result<Vec<i32>>;
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

    fn children(&self, pid: i32) -> Kids {
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

    fn all_pids(&self) -> io::Result<Vec<i32>> {
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
        Ok(pids)
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
        let kids = view.children(parent);
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
    for pid in view.all_pids()? {
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
mod tests {
    use super::super::test_fixtures::forked_tree::fork_tree;
    use super::super::{kill_descendants, kill_descendants_passes};
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    enum Entry {
        Live(i32, u64),
        Zombie(i32, u64),
        Gone,
        Errno(i32),
    }

    /// A programmable `/proc`: stats by pid, children by parent, and a log of
    /// every pid it was asked about.
    #[derive(Default)]
    struct Fake {
        stats: HashMap<i32, Entry>,
        kids: HashMap<i32, Vec<i32>>,
        unsupported: bool,
        threaded: HashSet<i32>,
        asked: RefCell<Vec<i32>>,
    }

    impl Fake {
        fn add(&mut self, pid: i32, parent: i32, entry: Entry) {
            self.stats.insert(pid, entry);
            self.kids.entry(parent).or_default().push(pid);
        }

        fn live(&mut self, pid: i32, parent: i32) {
            self.add(
                pid,
                parent,
                Entry::Live(parent, u64::from(pid.unsigned_abs())),
            );
        }
    }

    impl ProcView for Fake {
        fn stat(&self, pid: i32) -> io::Result<Option<Stat>> {
            self.asked.borrow_mut().push(pid);
            match self.stats.get(&pid) {
                Some(&Entry::Live(parent, start)) => Ok(Some(Stat {
                    parent,
                    zombie: false,
                    start,
                })),
                Some(&Entry::Zombie(parent, start)) => Ok(Some(Stat {
                    parent,
                    zombie: true,
                    start,
                })),
                Some(&Entry::Errno(e)) => Err(io::Error::from_raw_os_error(e)),
                Some(Entry::Gone) | None => Ok(None),
            }
        }

        fn has_other_tasks(&self, pid: i32) -> io::Result<bool> {
            Ok(self.threaded.contains(&pid))
        }

        fn children(&self, pid: i32) -> Kids {
            self.asked.borrow_mut().push(pid);
            Kids {
                pids: self.kids.get(&pid).cloned().unwrap_or_default(),
                error: None,
                unsupported: self.unsupported,
            }
        }

        fn all_pids(&self) -> io::Result<Vec<i32>> {
            Ok(self.stats.keys().copied().collect())
        }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    fn pids(pass: &Pass) -> Vec<i32> {
        let mut pids: Vec<i32> = pass.live.iter().map(|&(pid, _)| pid).collect();
        pids.sort_unstable();
        pids
    }

    #[test]
    fn an_unreadable_pid_does_not_hide_the_descendants_below_it() {
        let mut fake = Fake::default();
        fake.add(2, 1, Entry::Errno(libc::EIO));
        fake.live(3, 2);
        fake.live(4, 1);
        let pass = walk(1, far(), &fake).unwrap();
        assert_eq!(pids(&pass), [3, 4]);
        assert!(!pass.complete);
        assert_eq!(pass.first_error.unwrap().raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn the_fallback_scan_also_survives_an_unreadable_pid_but_cannot_see_below_it() {
        let mut fake = Fake {
            unsupported: true,
            ..Fake::default()
        };
        fake.add(2, 1, Entry::Errno(libc::EMFILE));
        fake.live(3, 2);
        fake.live(4, 1);
        let pass = live_members_via(1, far(), &fake).unwrap();
        // Parent links are read from each pid's own stat: what hangs below the
        // unreadable pid is unreachable, which the incomplete flag reports.
        assert_eq!(pids(&pass), [4]);
        assert!(!pass.complete);
        assert_eq!(pass.first_error.unwrap().raw_os_error(), Some(libc::EMFILE));
    }

    #[test]
    fn a_sweep_kills_what_an_incomplete_pass_found_and_stays_incomplete() {
        let mut fake = Fake::default();
        fake.add(2, 1, Entry::Errno(libc::EIO));
        fake.live(3, 2);
        fake.live(4, 1);
        let mut killed = Vec::new();
        let sweep = kill_descendants_passes(
            (1, 0),
            Instant::now() + Duration::from_millis(80),
            || live_members_via(1, far(), &fake),
            |pid, _| {
                killed.push(pid);
                Ok(())
            },
        );
        assert!(killed.contains(&3) && killed.contains(&4), "{killed:?}");
        assert!(!sweep.complete, "an unreadable pid must keep it incomplete");
        assert!(sweep.failures > 0);
        assert_eq!(sweep.first_error.unwrap().raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn an_incomplete_empty_pass_never_counts_toward_completion() {
        let pass = |complete| {
            Ok(Pass {
                live: vec![],
                complete,
                first_error: (!complete).then(|| io::Error::from_raw_os_error(libc::EIO)),
            })
        };
        let mut scans = vec![pass(true), pass(false), pass(true), pass(true)].into_iter();
        let mut count = 0;
        let sweep = kill_descendants_passes(
            (1, 0),
            far(),
            || {
                count += 1;
                scans.next().unwrap_or_else(|| pass(false))
            },
            |_, _| Ok(()),
        );
        assert!(sweep.complete);
        assert_eq!(count, 4, "an incomplete pass sat inside the pair");
        assert_eq!(sweep.failures, 1);
        // Never anything but incomplete passes: never complete.
        let sweep = kill_descendants_passes(
            (1, 0),
            Instant::now() + Duration::from_millis(60),
            || pass(false),
            |_, _| Ok(()),
        );
        assert!(!sweep.complete);
    }

    #[test]
    fn a_deep_tree_is_found_and_an_unrelated_process_is_never_read() {
        let mut fake = Fake::default();
        for pid in 2..=7 {
            fake.live(pid, pid - 1);
        }
        fake.live(8, 3);
        fake.add(9, 4, Entry::Zombie(4, 9));
        fake.add(10, 4, Entry::Gone);
        fake.live(99, 98); // unrelated: 98 is not in the tree
        fake.live(100, 1000);
        let pass = walk(1, far(), &fake).unwrap();
        assert_eq!(pids(&pass), [2, 3, 4, 5, 6, 7, 8]);
        assert!(pass.complete && pass.first_error.is_none());
        let asked = fake.asked.borrow();
        assert!(![98, 99, 100, 1000].iter().any(|p| asked.contains(p)));
    }

    #[test]
    fn a_zombie_with_running_threads_is_live_and_walked_through() {
        let mut fake = Fake::default();
        fake.add(2, 1, Entry::Zombie(1, 2));
        fake.threaded.insert(2);
        fake.live(3, 2);
        let pass = walk(1, far(), &fake).unwrap();
        assert_eq!(pids(&pass), [2, 3]);
    }

    #[test]
    fn without_children_files_the_walk_defers_to_the_full_scan() {
        let mut fake = Fake {
            unsupported: true,
            ..Fake::default()
        };
        fake.live(2, 1);
        fake.live(3, 2);
        fake.live(50, 49);
        assert!(walk(1, far(), &fake).is_none());
        let pass = live_members_via(1, far(), &fake).unwrap();
        assert_eq!(pids(&pass), [2, 3]);
        assert!(pass.complete);
    }

    #[test]
    fn a_pass_past_its_deadline_is_incomplete_without_an_error() {
        let mut fake = Fake::default();
        fake.live(2, 1);
        let past = Instant::now() - Duration::from_secs(1);
        let pass = live_members_via(1, past, &fake).unwrap();
        assert!(!pass.complete && pass.first_error.is_none());
    }

    /// The forker is a non-leader thread: the leader's own `children` file
    /// does not list the grandchild.
    extern "C" fn fork_then_pause(_: *mut libc::c_void) -> *mut libc::c_void {
        // SAFETY: fork/prctl/close_range/pause take integers only. Runs in a forked fixture child; the grandchild dies with this thread (PDEATHSIG) and holds no inherited fds.
        unsafe {
            if libc::fork() == 0 {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
            }
            loop {
                libc::pause();
            }
        }
    }

    fn thread_forks_a_child() {
        // SAFETY: pthread_create gets a valid out-pointer, null attrs, an `extern "C"` entry point that never returns, and a null arg; it runs in a child forked from the threaded harness, as in the zombie-leader fixture.
        unsafe {
            let mut thread: libc::pthread_t = 0;
            libc::pthread_create(
                &mut thread,
                std::ptr::null(),
                fork_then_pause,
                std::ptr::null_mut(),
            );
        }
    }

    #[test]
    fn a_child_of_a_non_leader_thread_is_found_and_killed() {
        let tree = fork_tree(thread_forks_a_child);
        let (root, child) = (tree.root, tree.child);
        let start = Instant::now();
        let mut found = Vec::new();
        while found.len() < 2 && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
            let pass = live_members_via(root, far(), &Procfs).unwrap();
            found = pids(&pass);
        }
        assert_eq!(found.len(), 2, "child and grandchild: {found:?}");
        let grandchild = found.iter().copied().find(|&p| p != child).unwrap();
        let leader_kids = std::fs::read_to_string(format!("/proc/{child}/task/{child}/children"))
            .unwrap_or_default();
        assert!(
            !leader_kids
                .split_whitespace()
                .any(|p| p == grandchild.to_string()),
            "fixture invalid: the grandchild hangs off the leader"
        );
        // Not the fallback in disguise, wherever this kernel has the files.
        if std::path::Path::new(&format!("/proc/{child}/task/{child}/children")).exists() {
            assert!(walk(root, far(), &Procfs).is_some());
        }
        let swept = kill_descendants(root, Duration::from_secs(3));
        assert!(swept.complete);
        let start = Instant::now();
        while std::path::Path::new(&format!("/proc/{grandchild}/task")).exists()
            && super::super::parent_and_zombie(grandchild).is_some_and(|(_, z)| !z)
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            super::super::parent_and_zombie(grandchild).is_none_or(|(_, zombie)| zombie),
            "the thread's child survived the sweep"
        );
    }
}
