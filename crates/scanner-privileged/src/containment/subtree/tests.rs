use super::super::kill_descendants_passes;
use super::super::test_fixtures::forked_tree::fork_tree;
use super::super::test_fixtures::signal as fixture_signal;
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
    /// Successive `children` answers for a pid, before `kids` applies.
    script: RefCell<HashMap<i32, std::collections::VecDeque<Vec<i32>>>>,
    /// Pids whose number is reused by another process once their
    /// `children` has been read: `stat` then reports another start time.
    recycle_on_children: HashSet<i32>,
    recycled: RefCell<HashSet<i32>>,
    /// Each `stat` takes this long.
    slow_stat: Duration,
    /// From its n-th `stat` call on, a pid reports another start time.
    bump_from_call: HashMap<i32, usize>,
    stat_calls: RefCell<HashMap<i32, usize>>,
}

impl Fake {
    fn add(&mut self, pid: i32, parent: i32, entry: Entry) {
        self.stats.insert(pid, entry);
        self.kids.entry(parent).or_default().push(pid);
    }

    /// The root's own stat, which the walk reads to pin its identity.
    fn root(&mut self) {
        self.stats.insert(1, Entry::Live(0, 1));
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
        std::thread::sleep(self.slow_stat);
        let call = {
            let mut calls = self.stat_calls.borrow_mut();
            let call = calls.entry(pid).or_default();
            *call += 1;
            *call
        };
        let bumped = self.recycled.borrow().contains(&pid)
            || self.bump_from_call.get(&pid).is_some_and(|&n| call >= n);
        let bump = if bumped { 1000 } else { 0 };
        match self.stats.get(&pid) {
            Some(&Entry::Live(parent, start)) => Ok(Some(Stat {
                parent,
                zombie: false,
                start: start + bump,
            })),
            Some(&Entry::Zombie(parent, start)) => Ok(Some(Stat {
                parent,
                zombie: true,
                start: start + bump,
            })),
            Some(&Entry::Errno(e)) => Err(io::Error::from_raw_os_error(e)),
            Some(Entry::Gone) | None => Ok(None),
        }
    }

    fn has_other_tasks(&self, pid: i32) -> io::Result<bool> {
        Ok(self.threaded.contains(&pid))
    }

    fn children(&self, pid: i32, _deadline: Instant) -> Kids {
        self.asked.borrow_mut().push(pid);
        if self.recycle_on_children.contains(&pid) {
            self.recycled.borrow_mut().insert(pid);
        }
        let scripted = self
            .script
            .borrow_mut()
            .get_mut(&pid)
            .and_then(|q| q.pop_front());
        Kids {
            pids: scripted.unwrap_or_else(|| self.kids.get(&pid).cloned().unwrap_or_default()),
            unsupported: self.unsupported,
            ..Kids::default()
        }
    }

    fn all_pids(&self, _deadline: Instant) -> io::Result<Option<Vec<i32>>> {
        Ok(Some(self.stats.keys().copied().collect()))
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
fn an_unreadable_pid_does_not_hide_its_siblings_or_their_descendants() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Errno(libc::EIO));
    fake.live(4, 1);
    fake.live(3, 4);
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
    fake.root();
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
    fake.root();
    fake.add(2, 1, Entry::Errno(libc::EIO));
    fake.live(4, 1);
    fake.live(3, 4);
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
    fake.root();
    for pid in 2..=7 {
        fake.live(pid, pid - 1);
    }
    fake.live(8, 3);
    fake.add(9, 4, Entry::Zombie(4, 9));
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
    fake.root();
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
    fake.root();
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
    fake.root();
    fake.live(2, 1);
    let past = Instant::now() - Duration::from_secs(1);
    let pass = live_members_via(1, past, &fake).unwrap();
    assert!(!pass.complete && pass.first_error.is_none());
}

#[test]
fn a_listed_child_whose_own_parent_differs_is_not_recorded_or_walked() {
    // 2 exited and its number went to an unrelated process (parent 77).
    let mut fake = Fake::default();
    fake.root();
    fake.kids.insert(1, vec![2]);
    fake.stats.insert(2, Entry::Live(77, 2));
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert!(pids(&pass).is_empty(), "{:?}", pass.live);
    assert!(!pass.complete);
    assert!(!fake.asked.borrow().contains(&3), "walked below a stranger");
}

#[test]
fn a_node_whose_number_is_reused_before_its_list_is_read_authorises_nothing() {
    let mut fake = Fake {
        recycle_on_children: HashSet::from([2]),
        ..Fake::default()
    };
    fake.root();
    fake.live(2, 1);
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2], "what 2's list said is discarded");
    assert!(!pass.complete);
    let mut fake = Fake {
        recycle_on_children: HashSet::from([1]),
        ..Fake::default()
    };
    fake.root();
    fake.live(2, 1);
    let pass = walk(1, far(), &fake).unwrap();
    assert!(pids(&pass).is_empty() && !pass.complete);
}

/// A node exits and its children reparent onto the root (the scanner is a
/// subreaper) after the root's list was read: the re-read finds them.
#[test]
fn an_orphan_reparented_onto_the_root_after_its_list_was_read_is_found() {
    let mut fake = Fake::default();
    fake.root();
    fake.stats.insert(2, Entry::Live(1, 2));
    fake.stats.insert(3, Entry::Live(1, 3));
    fake.script
        .borrow_mut()
        .insert(1, vec![vec![2], vec![2, 3]].into());
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2, 3]);
    assert!(pass.complete);
}

/// Each generation forks the next and exits, so a new orphan lands on the
/// root between every two reads: never a complete-empty sweep.
#[test]
fn a_fork_relay_never_yields_a_complete_empty_sweep() {
    let mut fake = Fake::default();
    fake.root();
    for pid in 10..400 {
        fake.stats
            .insert(pid, Entry::Live(1, u64::from(pid.unsigned_abs())));
    }
    let mut next = 10;
    let mut killed = Vec::new();
    let sweep = kill_descendants_passes(
        (1, 1),
        Instant::now() + Duration::from_millis(80),
        || {
            // Per pass: the root's list is empty at first, and an orphan
            // lands on it right after (its parent exited meanwhile).
            fake.script
                .borrow_mut()
                .insert(1, vec![vec![], vec![next]].into());
            next += 1;
            live_members_via(1, far(), &fake)
        },
        |pid, _| {
            killed.push(pid);
            Ok(())
        },
    );
    assert!(!killed.is_empty(), "the relay was never seen");
    assert!(!sweep.complete);
}

#[test]
fn a_root_list_that_keeps_changing_is_incomplete_at_the_round_cap() {
    let mut fake = Fake::default();
    fake.root();
    let mut reads = Vec::new();
    for pid in 10..60 {
        fake.stats
            .insert(pid, Entry::Live(1, u64::from(pid.unsigned_abs())));
        reads.push((10..=pid).collect());
    }
    fake.script.borrow_mut().insert(1, reads.into());
    let pass = walk(1, far(), &fake).unwrap();
    assert!(!pass.complete);
    assert!(!pass.live.is_empty());
}

/// P and its listed child C both exit and their numbers are reused between
/// P's start-time check and the admission of C: the check after the
/// admissions catches it, and nothing P listed is authorised.
#[test]
fn a_node_replaced_while_its_children_are_admitted_authorises_nothing() {
    let mut fake = Fake {
        // 2's stats: 1st = its admission, 2nd = the check before its
        // children are admitted, 3rd = the check after.
        bump_from_call: HashMap::from([(2, 3)]),
        ..Fake::default()
    };
    fake.root();
    fake.live(2, 1);
    fake.live(3, 2);
    fake.live(4, 3);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2], "{:?}", pass.live);
    assert!(!pass.complete);
    assert!(!fake.asked.borrow().contains(&4), "walked below 3");
}

/// Relay A -> B -> C with the root reaping: a listed child that vanishes may
/// have forked first, so the pass cannot look complete.
#[test]
fn a_listed_child_that_vanishes_keeps_the_pass_incomplete() {
    let mut fake = Fake::default();
    fake.root();
    fake.stats.insert(2, Entry::Gone);
    fake.stats.insert(3, Entry::Gone);
    fake.script
        .borrow_mut()
        .insert(1, vec![vec![2], vec![3]].into());
    let pass = walk(1, far(), &fake).unwrap();
    assert!(pass.live.is_empty());
    assert!(!pass.complete);
}

#[test]
fn a_dead_node_is_not_walked() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert!(pids(&pass).is_empty() && pass.complete);
    assert!(!fake.asked.borrow().contains(&3));
}

fn parsed(src: impl std::io::Read, deadline: Instant) -> Kids {
    let mut kids = Kids::default();
    parse_children(src, deadline, &mut kids);
    kids
}

#[test]
fn an_oversized_children_list_is_truncated_without_an_error() {
    let text = "1 ".repeat(MAX_CHILDREN_BYTES as usize);
    let kids = parsed(std::io::Cursor::new(text), far());
    assert!(kids.truncated && kids.error.is_none());
    let kids = parsed(std::io::Cursor::new("4 5 6\n"), far());
    assert_eq!(kids.pids, [4, 5, 6]);
    assert!(!kids.truncated);
}

#[test]
fn parsing_a_long_children_list_stops_at_the_deadline() {
    let text = "7 ".repeat(100_000);
    let past = Instant::now() - Duration::from_secs(1);
    let kids = parsed(std::io::Cursor::new(text), past);
    assert!(kids.truncated && kids.error.is_none());
    assert!(kids.pids.len() < 1000, "{}", kids.pids.len());
}

#[test]
fn the_deadline_is_checked_per_child() {
    let mut fake = Fake {
        slow_stat: Duration::from_millis(30),
        ..Fake::default()
    };
    fake.root();
    for pid in 2..12 {
        fake.live(pid, 1);
    }
    let pass = walk(1, Instant::now() + Duration::from_millis(100), &fake).unwrap();
    assert!(!pass.complete && pass.first_error.is_none());
    let stats = fake.asked.borrow().iter().filter(|&&p| p != 1).count();
    assert!(stats < 8, "kept reading past the bound: {stats}");
}

/// A flag in a `MAP_SHARED` page (a plain static would not cross the fixture's
/// fork, and the fixture closes every fd, so no pipe): the forker thread sets
/// it to 1 once its `fork` has returned, so [`ChildrenOf`] never scans before
/// the grandchild exists.
static FORKED: std::sync::atomic::AtomicPtr<std::sync::atomic::AtomicU32> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn mark_forked() {
    let flag = FORKED.load(std::sync::atomic::Ordering::SeqCst);
    if !flag.is_null() {
        // SAFETY: a non-null FORKED points to a live MAP_SHARED page that the test unmaps only after its guard saw this flag or timed out; an AtomicU32 store needs nothing else.
        unsafe { (*flag).store(1, std::sync::atomic::Ordering::SeqCst) };
    }
}

/// The forker is a non-leader thread: the leader's own `children` file does
/// not list the grandchild. The grandchild dies with the forking THREAD
/// (PDEATHSIG), which stays alive while the sweep runs: only the sweep's
/// signal (or the guard's, or the thread's death) ends it. Residual: if the
/// thread died before the grandchild's `prctl`, the grandchild is left alive;
/// the guard waits for `mark_forked` and kills whatever is below the child.
extern "C" fn fork_then_pause(_: *mut libc::c_void) -> *mut libc::c_void {
    // SAFETY: fork/prctl/close_range/pause take integers only. Runs in a forked fixture child; the grandchild holds no inherited fds.
    unsafe {
        if libc::fork() == 0 {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
        } else {
            mark_forked();
        }
        loop {
            libc::pause();
        }
    }
}

/// Accepted exception to `fork_tree`'s async-signal-safe contract:
/// `pthread_create` in the forked fixture child, as the zombie-leader
/// fixture does; a hang there is contained by the fork tree's PDEATHSIG.
fn thread_forks_a_child() {
    // SAFETY: pthread_create gets a valid out-pointer, null attrs, an `extern "C"` entry point that never returns, and a null arg.
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

/// On drop: waits (bounded) for the forker's flag, kills every live process
/// whose parent is `.0` (the fixture's child) through a pidfd and only while
/// the parent is still `.0`, then unmaps the flag page. Declare it after the
/// `ForkedTree` so it runs first, while `.0` is alive.
struct ChildrenOf(i32, *mut libc::c_void);
impl ChildrenOf {
    /// Map the flag page; call before `fork_tree`.
    fn flag_page() -> *mut libc::c_void {
        // SAFETY: an anonymous shared mapping of one page: no fd, no address hint.
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(page, libc::MAP_FAILED);
        FORKED.store(page.cast(), std::sync::atomic::Ordering::SeqCst);
        page
    }
}
impl Drop for ChildrenOf {
    fn drop(&mut self) {
        let flag = self.1.cast::<std::sync::atomic::AtomicU32>();
        let start = Instant::now();
        // SAFETY: `self.1` is the live page from `flag_page`, unmapped below only.
        while unsafe { (*flag).load(std::sync::atomic::Ordering::SeqCst) } == 0
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Ok(dir) = std::fs::read_dir("/proc") {
            for pid in dir
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
            {
                let ours = |p| {
                    super::super::parent_and_zombie(p)
                        .is_some_and(|(parent, zombie)| parent == self.0 && !zombie)
                };
                if ours(pid) {
                    fixture_signal::kill_verified(pid, ours);
                }
            }
        }
        FORKED.store(std::ptr::null_mut(), std::sync::atomic::Ordering::SeqCst);
        // SAFETY: unmaps the page mapped by `flag_page`, once, after the last use above.
        unsafe { libc::munmap(self.1, 4096) };
    }
}

#[test]
fn a_child_of_a_non_leader_thread_is_found_and_killed() {
    let page = ChildrenOf::flag_page();
    let tree = fork_tree(thread_forks_a_child);
    let (root, child) = (tree.root, tree.child);
    let _cleanup = ChildrenOf(child, page);
    let start = Instant::now();
    let mut found = Vec::new();
    while found.len() < 2 && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
        let pass = live_members_via(root, far(), &Procfs).unwrap();
        found = pids(&pass);
    }
    assert_eq!(found.len(), 2, "child and grandchild: {found:?}");
    let grandchild = found.iter().copied().find(|&p| p != child).unwrap();
    let leader_kids =
        std::fs::read_to_string(format!("/proc/{child}/task/{child}/children")).unwrap_or_default();
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
    let alive = |p| super::super::parent_and_zombie(p).is_some_and(|(_, zombie)| !zombie);
    assert!(alive(grandchild), "fixture invalid: it died on its own");
    // The real pass and the real pidfd kill, except that the child is
    // spared so the grandchild stays below it (the fixture root is no
    // subreaper): only the sweep can end the grandchild.
    let root_start = super::super::read_stat_with_start(root)
        .unwrap()
        .unwrap()
        .start;
    let deadline = Instant::now() + Duration::from_millis(800);
    let sweep = kill_descendants_passes(
        (root, root_start),
        deadline,
        || super::super::live_members(root, deadline),
        |pid, tree| {
            if pid == child {
                Ok(())
            } else {
                super::super::kill_if_descendant(pid, tree)
            }
        },
    );
    assert!(!sweep.complete, "the spared child keeps the sweep open");
    assert!(!alive(grandchild), "the thread's child survived the sweep");
    assert!(alive(child), "the sweep killed what the test spared");
}
