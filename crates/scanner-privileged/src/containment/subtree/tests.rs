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
    /// From its n-th `stat` call on, a zombie pid reports a live process (a
    /// thread exec'd and took over the leader's pid).
    revive_from_call: HashMap<i32, usize>,
    /// From its n-th `has_other_tasks` call on, a pid has another task (a
    /// live worker appeared under the zombie leader).
    threaded_from_call: HashMap<i32, usize>,
    task_calls: RefCell<HashMap<i32, usize>>,
    /// From its n-th `stat` call on, a pid has vanished.
    vanish_from_call: HashMap<i32, usize>,
    /// From its n-th `children` call on, a pid reports no `children` file.
    unsupported_from_call: HashMap<i32, usize>,
    children_calls: RefCell<HashMap<i32, usize>>,
    /// Every `children` answer is flagged truncated.
    truncated: bool,
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
        let revived = self.revive_from_call.get(&pid).is_some_and(|&n| call >= n);
        if self.vanish_from_call.get(&pid).is_some_and(|&n| call >= n) {
            return Ok(None);
        }
        match self.stats.get(&pid) {
            Some(&Entry::Live(parent, start)) => Ok(Some(Stat {
                parent,
                zombie: false,
                start: start + bump,
            })),
            Some(&Entry::Zombie(parent, start)) => Ok(Some(Stat {
                parent,
                zombie: !revived,
                start: start + bump,
            })),
            Some(&Entry::Errno(e)) => Err(io::Error::from_raw_os_error(e)),
            Some(Entry::Gone) | None => Ok(None),
        }
    }

    fn has_other_tasks(&self, pid: i32) -> io::Result<bool> {
        let call = {
            let mut calls = self.task_calls.borrow_mut();
            let call = calls.entry(pid).or_default();
            *call += 1;
            *call
        };
        let late = self
            .threaded_from_call
            .get(&pid)
            .is_some_and(|&n| call >= n);
        Ok(self.threaded.contains(&pid) || late)
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
        let call = {
            let mut calls = self.children_calls.borrow_mut();
            let call = calls.entry(pid).or_default();
            *call += 1;
            *call
        };
        let unsupported_now = self
            .unsupported_from_call
            .get(&pid)
            .is_some_and(|&n| call >= n);
        Kids {
            pids: scripted.unwrap_or_else(|| self.kids.get(&pid).cloned().unwrap_or_default()),
            unsupported: self.unsupported || unsupported_now,
            truncated: self.truncated,
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
            dead: vec![],
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

/// A zombie that vanishes while its death is re-checked still links its
/// children to the root: they are found, but the pass is not complete.
#[test]
fn the_fallback_keeps_a_zombie_that_vanishes_on_revalidation_as_an_intermediary() {
    let mut fake = Fake {
        unsupported: true,
        ..Fake::default()
    };
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.vanish_from_call.insert(2, 2);
    fake.live(3, 2);
    let pass = live_members_via(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [3]);
    assert!(!pass.complete);
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

/// A true zombie is walked through (its children are found) and reported in
/// `dead`, never in `live`, so it cannot block completion.
#[test]
fn a_dead_node_is_walked_but_not_live() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [3]);
    assert_eq!(pass.dead, [(2, 2)]);
    assert!(pass.complete);
    assert!(fake.asked.borrow().contains(&3));
}

/// The is_dead sequence is fooled (zombie, no tasks, same zombie, no tasks)
/// yet the node has a live child: the child is found and the node is still
/// handed to the kill step.
#[test]
fn a_misclassified_node_is_walked_and_signalled() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [3]);
    assert_eq!(pass.dead, [(2, 2)]);
    let killed = RefCell::new(Vec::new());
    let mut scans = vec![pass].into_iter();
    kill_descendants_passes(
        (1, 1),
        far(),
        || {
            // After the walk, the node is a childless misclassified one: the
            // empty-pass kill path must still signal it.
            Ok(scans.next().unwrap_or_else(|| Pass {
                live: vec![],
                dead: vec![(2, 2)],
                complete: true,
                first_error: None,
            }))
        },
        |pid, tree| {
            assert!(tree.contains(&(2, 2)));
            killed.borrow_mut().push(pid);
            Ok(())
        },
    );
    let mut killed = killed.into_inner();
    killed.sort_unstable();
    assert_eq!(killed.iter().filter(|&&p| p == 3).count(), 1);
    // Pass 1 (as a dead node) and every later empty pass signal pid 2.
    assert!(
        killed.iter().filter(|&&p| p == 2).count() >= 2,
        "{killed:?}"
    );
}

/// A dead node reaped before its list or identity is re-read hands its
/// children to the root: that is no doubt, the pass stays complete.
#[test]
fn a_dead_node_that_vanishes_when_expanded_keeps_the_pass_complete() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    // is_dead's second stat is call 2; the expand's two stats are 3 and 4.
    fake.vanish_from_call.insert(2, 3);
    let pass = walk(1, far(), &fake).unwrap();
    assert!(pass.live.is_empty());
    assert_eq!(pass.dead, [(2, 2)]);
    assert!(fake
        .children_calls
        .borrow()
        .get(&2)
        .is_some_and(|&n| n >= 1));
    assert!(pass.complete, "{:?}", pass.first_error);

    // Vanishing only between the two expand stats is no doubt either.
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.vanish_from_call.insert(2, 4);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pass.dead, [(2, 2)]);
    assert!(fake
        .children_calls
        .borrow()
        .get(&2)
        .is_some_and(|&n| n >= 1));
    assert!(pass.complete, "{:?}", pass.first_error);

    // A child that shows up under the root on the re-read lands in `live`.
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.stats.insert(3, Entry::Live(1, 3));
    fake.vanish_from_call.insert(2, 3);
    fake.script
        .borrow_mut()
        .insert(1, vec![vec![2], vec![2, 3]].into());
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [3]);
    assert_eq!(pass.dead, [(2, 2)]);
    assert!(pass.complete, "{:?}", pass.first_error);
}

type Members = &'static [(i32, u64)];

/// Scripted passes `(live, dead)`, all complete; the last one repeats. `calls`
/// counts the scans.
fn scripted<'a>(
    passes: &'static [(Members, Members)],
    calls: &'a std::cell::Cell<usize>,
) -> impl FnMut() -> io::Result<Pass> + 'a {
    move || {
        let (live, dead) = passes[calls.get().min(passes.len() - 1)];
        calls.set(calls.get() + 1);
        Ok(Pass {
            live: live.to_vec(),
            dead: dead.to_vec(),
            complete: true,
            first_error: None,
        })
    }
}

/// A dead node first seen in the completing pass was signalled after the walk
/// that produced its emptiness evidence: that pass cannot count.
#[test]
fn a_pass_with_a_newly_found_dead_node_does_not_count_toward_completion() {
    const A: (i32, u64) = (2, 2);
    const X: (i32, u64) = (3, 3);
    static PASSES: [(Members, Members); 3] = [(&[], &[A]), (&[], &[X]), (&[], &[X])];
    let calls = std::cell::Cell::new(0);
    let killed = RefCell::new(Vec::new());
    let sweep = kill_descendants_passes((1, 1), far(), scripted(&PASSES, &calls), |pid, _| {
        killed.borrow_mut().push(pid);
        Ok(())
    });
    assert!(sweep.complete && sweep.failures == 0);
    assert!(calls.get() >= 3, "completed after {} passes", calls.get());
    assert!(killed.borrow().contains(&2) && killed.borrow().contains(&3));
}

/// A true zombie reports the same (pid, start) every pass: it costs one extra
/// pass and still cannot block completion.
#[test]
fn a_true_zombie_seen_in_every_pass_completes_after_the_third() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2)])];
    let calls = std::cell::Cell::new(0);
    let sweep = kill_descendants_passes((1, 1), far(), scripted(&PASSES, &calls), |_, _| Ok(()));
    assert!(sweep.complete && sweep.failures == 0);
    assert_eq!(calls.get(), 3);
}

/// A signal that failed proves nothing: the pass does not count and the node
/// is not remembered as signalled.
#[test]
fn a_failed_signal_on_a_dead_node_delays_completion_and_surfaces() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2)])];
    let calls = std::cell::Cell::new(0);
    let attempts = std::cell::Cell::new(0);
    let sweep = kill_descendants_passes((1, 1), far(), scripted(&PASSES, &calls), |_, _| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            Err(io::Error::from_raw_os_error(libc::EMFILE))
        } else {
            Ok(())
        }
    });
    // Pass 1 fails, pass 2 signals, pass 3 and 4 count.
    assert!(sweep.complete);
    assert_eq!(calls.get(), 4);
    assert_eq!(sweep.failures, 1);
    assert_eq!(
        sweep.first_error.unwrap().raw_os_error(),
        Some(libc::EMFILE)
    );

    // A signal that keeps failing never completes.
    let calls = std::cell::Cell::new(0);
    let sweep = kill_descendants_passes(
        (1, 1),
        Instant::now() + Duration::from_millis(60),
        scripted(&PASSES, &calls),
        |_, _| Err(io::Error::from_raw_os_error(libc::EMFILE)),
    );
    assert!(!sweep.complete && sweep.failures > 0);
}

/// An unreaped zombie is signalled but two consecutive passes that find only
/// it still complete the sweep.
#[test]
fn a_true_zombie_is_signalled_and_does_not_block_completion() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    let killed = RefCell::new(Vec::new());
    let sweep = kill_descendants_passes(
        (1, 1),
        far(),
        || Ok(walk(1, far(), &fake).unwrap()),
        |pid, _| {
            killed.borrow_mut().push(pid);
            Ok(())
        },
    );
    assert!(sweep.complete && sweep.failures == 0);
    assert!(killed.borrow().contains(&2));
}

/// A zombie leader whose last other thread exec'd becomes a live process
/// under the same pid and start time: the stale zombie flag must not hide
/// what hangs below it.
#[test]
fn a_zombie_revived_by_exec_after_the_task_check_is_live_and_walked() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.revive_from_call.insert(2, 2);
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2, 3]);
}

/// A zombie leader's worker exec'd into the leader pid and a new worker was
/// created before the main thread exited again: the second stat matches the
/// first (same parent and start) and only a task listing read after it sees
/// the live worker.
#[test]
fn a_zombie_that_gains_a_worker_after_the_second_stat_is_live_and_walked() {
    let mut fake = Fake::default();
    fake.root();
    fake.add(2, 1, Entry::Zombie(1, 2));
    fake.threaded_from_call.insert(2, 2);
    fake.live(3, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2, 3]);
}

/// The root's list is readable at first and unsupported on the re-read: the
/// re-read dropped every pid, so the pass is no evidence of an empty subtree.
#[test]
fn a_root_list_unsupported_only_on_the_reread_fails_the_pass() {
    let mut fake = Fake::default();
    fake.root();
    fake.live(2, 1);
    fake.unsupported_from_call.insert(1, 2);
    let pass = walk(1, far(), &fake).unwrap();
    assert!(!pass.complete);
    assert!(pass.first_error.is_some());
}

/// A truncated list still admits what was read; the pass stays incomplete.
#[test]
fn a_truncated_list_still_admits_its_pids_but_is_incomplete() {
    let mut fake = Fake {
        truncated: true,
        ..Fake::default()
    };
    fake.root();
    fake.live(2, 1);
    fake.live(3, 1);
    let pass = walk(1, far(), &fake).unwrap();
    assert_eq!(pids(&pass), [2, 3]);
    assert!(!pass.complete);
}

/// 50k siblings: the duplicate check must not be quadratic.
#[test]
fn admitting_a_large_sibling_list_is_linear() {
    let mut fake = Fake::default();
    fake.root();
    for pid in 2..50_002 {
        fake.live(pid, 1);
    }
    let start = Instant::now();
    let pass = walk(1, Instant::now() + Duration::from_secs(60), &fake).unwrap();
    assert_eq!(pass.live.len(), 50_000);
    assert!(pass.complete);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
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

/// Over the cap the readable prefix is kept, cut at its last whitespace so a
/// pid cut in half is never parsed.
#[test]
fn an_oversized_list_keeps_the_pids_read_before_the_cap_but_not_a_cut_one() {
    let text = format!("4 5 {}", "9".repeat(MAX_CHILDREN_BYTES as usize));
    let kids = parsed(std::io::Cursor::new(text), far());
    assert!(kids.truncated && kids.error.is_none());
    assert_eq!(kids.pids, [4, 5]);
    // The cap lands exactly after a whole pid: it is kept.
    let mut text = "6 ".repeat(MAX_CHILDREN_BYTES as usize / 2);
    text.push_str("7 8");
    let kids = parsed(std::io::Cursor::new(text), far());
    assert!(kids.truncated);
    assert_eq!(kids.pids.len(), MAX_CHILDREN_BYTES as usize / 2);
    assert!(kids.pids.iter().all(|&p| p == 6));
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
        // SAFETY: only called in the forked child, whose inherited MAP_SHARED mapping of the page stays mapped for its lifetime, whatever the parent unmaps. An AtomicU32 store needs nothing else.
        unsafe { (*flag).store(1, std::sync::atomic::Ordering::SeqCst) };
    }
}

/// The forker is a non-leader thread: the leader's own `children` file does
/// not list the grandchild. The grandchild dies with the forking THREAD
/// (PDEATHSIG), which stays alive while the sweep runs: only the sweep's
/// signal (or the guard's, or the thread's death) ends it. If the thread died
/// before the grandchild's `prctl` the signal would never come, so after it
/// the grandchild checks (signal 0, nothing is sent) that the forking thread
/// and process are still there and exits at once if not.
extern "C" fn fork_then_pause(_: *mut libc::c_void) -> *mut libc::c_void {
    // SAFETY: getpid/gettid/fork/prctl/tgkill(sig 0)/getppid/_exit/close_range/pause take integers only. Runs in a forked fixture child; the grandchild holds no inherited fds. tgkill with signal 0 only probes the grandchild's own forking thread, captured before the fork.
    unsafe {
        let tgid = libc::getpid();
        let tid = libc::syscall(libc::SYS_gettid) as libc::pid_t;
        if libc::fork() == 0 {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            let parent_gone =
                libc::getppid() != tgid || libc::syscall(libc::SYS_tgkill, tgid, tid, 0) != 0;
            if parent_gone {
                libc::_exit(0);
            }
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
