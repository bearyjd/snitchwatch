# Plan: #329 items 1-2 — scanner sweep fails closed, exit sweep cannot fail silently

Date: 2026-11-07. Crate: `scanner-privileged` (PRODUCTION kill / watchdog code, privileged).
Status: APPROVED 2026-11-07 with the recommendations: fail closed; exit code 3 on an incomplete exit sweep; alarm margin DESCENDANT_SWEEP + 5 s. PR 1 (item 1) in progress; merge held until the owner says the r16.1 cut is done. Source: tower security reviews of
32eca9a7..850c0cc9 (0 CRITICAL, 0 HIGH); issue #329. Mapped read-only against main f6790d7a.
Sequencing: r16.1 is being cut; this is built on branches and **not merged until the owner says
the cut is done** (the cut pins a reviewed SHA; tower fetches main itself).

## Item 1 — `kill_if_descendant_with` (containment.rs ~156-202)

Today: `pidfd_open` failing with anything but ESRCH (EMFILE, ENOSYS, EPERM/seccomp) discards the
errno, checks the target's parent in `/proc`, then `kill(pid, SIGKILL)` by bare number. The parent
check also compares against a numeric snapshot `tree = live ∪ {root}` built once per pass
(`kill_descendants_with` ~:278) and reused for every pid, so a recycled ancestor number can admit an
unrelated process even on the pidfd path. The test
`any_pidfd_failure_but_esrch_falls_back_to_a_parent_checked_kill` (~:365) pins the old behaviour.

Design:
1. **Fail closed.** Any `open` error other than ESRCH returns `Err(e)` (the errno is kept) without
   signalling. `kill_descendants_with` already counts a `kill` `Err` into `Sweep.failures` /
   `first_error`, so the sweep becomes incomplete instead of silently killing by bare pid.
2. **Pinned identities for ancestry.** Extend `read_stat` to also return the process start time
   (`/proc/<pid>/stat` field 22, index 19 after the closing paren). The pass snapshot records
   `(pid, starttime)` for every tree member. Before killing target T, re-read T's parent P and accept
   only if `(P, starttime(P))` is in the snapshot; a recycled number has a different start time. This
   adds no file descriptors (no new EMFILE exposure, unlike pinning every member with a pidfd). The
   target itself is still pinned by its pidfd and re-checked after the open.
3. Behaviour when nothing fails is unchanged.

Failing-first tests (injected `open`/`read_stat`/`scan`; no live signals):
- invert the test at ~:365: for ENOSYS, EPERM, EMFILE the result is `Err(errno)`, the child is alive
  and `kill` was never called (even when its parent is in the tree);
- `kill_descendants_with` with an `open` that fails EMFILE: `failures >= 1`, `complete == false`;
- a recycled ancestor (same pid number, different start time) does not admit the target;
- unchanged: ESRCH is Gone and skips; parent outside the tree is spared; real-process tests via
  `ScannerFixture`.

## Item 2 — exit sweep policy (command.rs ~219-256, main.rs ~228-279)

Today: `finish_process` runs the 2 s exit sweep, and if incomplete only `tracing::warn!`s to
**blocking** stderr; `Phase::Exiting` makes any later watchdog expiry a no-op and no hard timer
exists (the 540 s watchdog thread just becomes inert). So an escaped descendant (own pgid/session) can
survive silently and a blocked stderr write can hang the exit with nothing left to stop it.
`run()` returns 0/2 regardless of the sweep.

Design:
1. `finish_process_with` takes a sweep closure returning the `Sweep` and returns it (callers:
   `ExitGuard::drop`, the `exit(2)` path, tests).
2. **Non-blocking, bounded diagnostics.** The post-exit "exit sweep left descendants" note is written
   like `notify_watchdog_timeout` (`main.rs:78-100`): only if stderr is a FIFO, `O_NONBLOCK`, a single
   `write` shorter than `PIPE_BUF`, result ignored. No `tracing` call after the sweep.
3. **Hard alarm through exit.** On entering `Phase::Exiting`, arm `alarm(SWEEP + margin)` (default
   SIGALRM action terminates the process) so a hung exit cannot outlive the bound. Clean exits finish
   far sooner and never see it. This bounds the hang; it does **not** by itself kill escaped
   descendants (a cgroup/lifetime owner is the reliable fix and is out of scope here).
4. **Incomplete sweep is a failure** (see decision 1).

Failing-first tests: `finish_process_with` returns the incomplete `Sweep` from a fake sweep; the
diagnostic goes through the non-blocking writer (a full pipe does not block: fixture with a pipe
filled to capacity); the alarm is armed on `Exiting` and not on the success path before it (observe
via an injected `arm_alarm`); `ScannerFixture` re-exec: a descendant that ignores the sweep surfaces
the failure exit code, a clean run still exits 0; existing exit tests
(`main_waits_for_the_watchdog_to_finish_its_sweep`, `a_clean_exit_sweeps_a_successful_primitives_escapee`,
`a_watchdog_that_fires_after_exit_began_does_nothing`) stay green unmodified.

## Decisions for the owner
1. **Exit code on an incomplete exit sweep.** Today 0/2 whatever the sweep did. Options: (a) keep
   0/2, only the loud non-blocking marker + the alarm (no consumer-visible change); (b) a new code
   (e.g. 3) so callers (tower, polkit launcher) can see it. Recommendation: (b), but it is a
   consumer-visible contract change, so say so before I do it.
2. **Hosts without a usable `pidfd_open`** (kernel < 5.3 or a seccomp filter blocking it) now get an
   incomplete sweep every run instead of bare-pid kills. Bazzite kernels are far newer. Recommendation:
   accept (fail closed). Say if any target needs the fallback.
3. **Alarm margin.** Recommendation `DESCENDANT_SWEEP (2 s) + 5 s`.

## PRs and process
- PR 1: item 1 (fail closed + pinned ancestry). PR 2: item 2. Items 3-4 (LOW) stay on the issue.
- TDD, failing-first, `cargo` only on a throwaway `HOME`; never run binaries from `target/`.
- Review: independent Opus `security-reviewer` + `code-reviewer` on the exact head per PR; a MEDIUM+
  needs the owner's explicit exception. The tower redoes its own security review of the privileged
  change before repinning, so tell it the merged SHAs.
- Not merged until the owner confirms the r16.1 cut is finished.
