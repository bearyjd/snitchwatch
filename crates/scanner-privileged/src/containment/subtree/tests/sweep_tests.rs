use super::*;

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
        Ok(KillOutcome::Signalled)
    });
    assert!(sweep.complete && sweep.failures == 0);
    assert_eq!(calls.get(), 4, "completed after {} passes", calls.get());
    assert!(killed.borrow().contains(&2) && killed.borrow().contains(&3));
}

/// Runs the passes `PASSES` with `outcome(pass number from 1, pid)` as the
/// kill result; returns the sweep and the number of scans.
fn sweep_with_outcomes(
    passes: &'static [(Members, Members)],
    deadline: Instant,
    outcome: impl Fn(usize, i32) -> KillOutcome,
) -> (Sweep, usize) {
    let calls = std::cell::Cell::new(0);
    let sweep = kill_descendants_passes((1, 1), deadline, scripted(passes, &calls), |pid, _| {
        Ok(outcome(calls.get(), pid))
    });
    (sweep, calls.get())
}

/// A dead node whose ancestry check was skipped is not credited as signalled:
/// the two counting passes come only after the pass that really signalled it.
#[test]
fn an_unverified_dead_node_is_not_credited_until_it_is_signalled() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(3, 3)])];
    let (sweep, scans) = sweep_with_outcomes(&PASSES, far(), |pass, _| {
        if pass < 3 {
            KillOutcome::Unverified
        } else {
            KillOutcome::Signalled
        }
    });
    // Pass 3 signals; passes 4 and 5 count.
    assert!(sweep.complete);
    assert_eq!(scans, 5);
    assert_eq!(sweep.failures, 0);
    assert!(sweep.first_error.is_none());
}

/// A dead node that vanished is credited like a signalled one.
#[test]
fn a_gone_dead_node_counts_as_credited() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2)])];
    let (sweep, scans) = sweep_with_outcomes(&PASSES, far(), |_, _| KillOutcome::Gone);
    assert!(sweep.complete && sweep.failures == 0);
    assert_eq!(scans, 3);
}

/// Unverified is not a failure: it only keeps the sweep from completing.
#[test]
fn an_unverified_outcome_is_not_a_failure() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2)])];
    let deadline = Instant::now() + Duration::from_millis(60);
    let (sweep, _) = sweep_with_outcomes(&PASSES, deadline, |_, _| KillOutcome::Unverified);
    assert!(!sweep.complete);
    assert_eq!(sweep.failures, 0);
    assert!(sweep.first_error.is_none());
}

/// Chain root->C->B->A->D with auto-reaping: B and D are skipped (parent read
/// vanished) in turn. Pass 3 is the first to signal D, so the sweep needs two
/// more passes after it; it must not complete at pass 3.
#[test]
fn a_chain_of_skipped_dead_nodes_needs_two_counting_passes_after_the_last_signal() {
    const B: i32 = 2;
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2), (4, 4)])];
    let (sweep, scans) = sweep_with_outcomes(&PASSES, far(), |pass, pid| match (pass, pid == B) {
        (1, _) | (2, false) => KillOutcome::Unverified,
        _ => KillOutcome::Signalled,
    });
    assert!(sweep.complete);
    assert_eq!(
        scans, 5,
        "completed early on a credited-but-unsignalled node"
    );
    assert_eq!(sweep.failures, 0);
}

/// A true zombie reports the same (pid, start) every pass: it costs one extra
/// pass and still cannot block completion.
#[test]
fn a_true_zombie_seen_in_every_pass_completes_after_the_third() {
    static PASSES: [(Members, Members); 1] = [(&[], &[(2, 2)])];
    let calls = std::cell::Cell::new(0);
    let sweep = kill_descendants_passes((1, 1), far(), scripted(&PASSES, &calls), |_, _| {
        Ok(KillOutcome::Signalled)
    });
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
            Ok(KillOutcome::Signalled)
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
            Ok(KillOutcome::Signalled)
        },
    );
    assert!(sweep.complete && sweep.failures == 0);
    assert!(killed.borrow().contains(&2));
}
