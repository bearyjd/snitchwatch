---
title: "The root exit sweep treats a zombie as possibly alive: walk through it, kill it, and only count a pass after its kills were seen to land"
author: Snitchwatch
date: 2026-11-15
branch: main
cost: { minutes: 116, tokens: 1093264 }
id: l-9a0e402e
scope: repo
status: active
src: agent
touches:
  - crates/scanner-privileged/src/command.rs
  - crates/scanner-privileged/src/containment.rs
  - crates/scanner-privileged/src/containment/test_fixtures/marked_sleeps.rs
  - crates/scanner-privileged/src/containment/test_fixtures/scanner_fixture.rs
  - crates/scanner-privileged/src/containment/test_fixtures/signal.rs
  - HANDOFF.md
  - crates/snitchwatch-bridge/tests/ebpf_modules_shape.rs
---

In the code: crates/scanner-privileged/src/containment.rs (kill_descendants_passes, signal_pass, kill_all, kill_if_descendant_reading, KillOutcome) and containment/subtree.rs (the subtree walker), work in PR #362 for issue #329 item 3. Tests in containment/subtree/tests.rs and tests/sweep_tests.rs.

## Decided
- Whether a zombie is really dead cannot be made race-free from /proc reads: a worker thread can exec into the leader's pid, spawn another worker and exit, and every extra stat or task-list read can be beaten by one more step. Eleven codex rounds kept finding the next variant. So stop trying to prevent a wrong "dead" call and make it harmless.
- A node classified dead is still walked through (its children are admitted) and is still SIGKILLed through the same pidfd + (pid, start) ancestry check, but it is kept in its own list and never counted as live, so unreaped zombies cannot block completion.
- A pass counts toward the two-empty-passes completion rule only if every dead node in it was already signalled in an EARLIER pass and no signal failed (a process with SIGKILL pending cannot fork).
- The kill step reports Signalled, Gone or Unverified. Only a pidfd ESRCH is Gone. A vanished /proc stat under a held pidfd is Unverified (an exec handover can hide a live leader), and an Unverified node is neither credited nor a failure; it just delays completion by a pass.
- Children come from /proc/<pid>/task/<tid>/children (every task, since a non-leader thread's children hang off that thread), a child is admitted only if its OWN stat names the listing pid as parent, and the listing node's start time is re-checked before and after admitting its children, with the whole batch rolled back on a mismatch.
- The root (the scanner is a subreaper) is re-read until a re-read adds nothing, capped at 4 rounds.

## Tried and ruled out
- Skipping a dead node's subtree (my own first optimisation): worse than the code on main, which already walked through dead nodes.
- Adding more stat or task-list reads after the first zombie check: each one was defeated by one more exec/respawn step.
- Refusing to start when the scanner is not a session leader: would break hand-run scans.
- Counting a skipped kill as signalled because the function returned Ok(()): let a skipped node earn completion credit.

## Watch out
- Review a branch against a CURRENT origin/main: a branch cut before #361 made `git diff origin/main` look like it removed setsid, and codex reported it as a HIGH. Merge main in first; the PR diff's patch-id stays the same.
- Tell the reviewers what is accepted, or they re-report it: the full-/proc fallback's non-atomic snapshot (only used without CONFIG_PROC_CHILDREN) and the kernel documenting `children` as not exact unless tasks are stopped.
- A forked test fixture must not leak its grandchild: capture the forking thread's tid before fork and check it after PR_SET_PDEATHSIG; a pipe cannot be used because fork_tree closes every fd.
- A test that asserts a child is in state S races the child's own prctl; accept R or S.
- Headless Kirigami keyboard-probe QML tests flake on CI (two different ones on this PR, issue filed). Rerun the failed job; the tests are unrelated to scanner changes.
