# Watchdog test fixtures: pidfd-verified kills, /proc-independent cleanup (2026-11-07)

Issue #322, part 1. Scope: `crates/scanner-privileged`, `#[cfg(test)]` code
only (`containment.rs`, `command.rs`, `main.rs` tests). Production sweep and
watchdog code is unchanged.

## Problem
#318 made fixture signalling refuse pid or group <= 1. Three classes remain:
1. Identity: tests kill a *recorded* pid (read from a file or an earlier scan)
   by bare `kill(pid)`. If the process is gone and the pid recycled, an
   unrelated process is killed.
2. `MarkedSleeps::drop` only knows sleepers a successful `/proc` scan saw. If
   every scan fails before the first observation the list is empty and an
   escaped `setsid` shell (and its sleepers) survives a failing test.
3. `fork_tree` children `P` and `C` are paused forever; a killed harness
   leaves them orphaned. `with_scan` accepts the test process as the root.
Also `main.rs` (`watchdog_kills_its_group...`) does raw `kill(-pid)`/`kill(pid)`
after a `recv_timeout`, racing the reaper thread that owns the `Child`.

## Approach
(a) `fixture_signal::kill_verified_with(pid, open, send, identity)`:
1. refuse pid <= 1 (`Unsafe`);
2. `pidfd_open(pid)`: `ESRCH` is `Gone`, any other error is `Refused` (never a
   fallback to `kill(pid)`);
3. only now call `identity(pid)` (the pidfd pins the process, a recycled pid
   fails identity): false is `Mismatch`, nothing is sent;
4. `pidfd_send_signal` on that fd (`ESRCH` is `Gone`, other errors `Refused`).
`open`/`send` are injected so every branch is tested with fakes; the fake
`open` returns an `OwnedFd` of `/dev/null`, so nothing real is closed or
signalled. `kill_marked(pid, marker)` is the real-world wrapper: identity is
"cmdline contains the unique marker". `parent_is(ppid)` is the other predicate.

Call sites: `MarkedSleeps::drop` phase 1 (marker) and phase 2 (each scanned
descendant, parent must be `root` or another scanned pid); the `command.rs`
re-exec tests (marker passed to the fixture by env var, used as the argv of
its `sleep`s and as an extra argument of the fixture process, so a
fork-before-exec child also carries it); `main.rs`: poll the child with
`waitid(WNOWAIT)` on the test thread (no reaper thread), and on timeout
`killpg` its group while it is still unreaped, so its pid cannot be recycled.

(b) `MarkedSleeps` can be given a pid file the fixture script appends to
(`$$` of the escaped shell first, then each `$!`). `drop` reads it first and
kills every listed pid through `kill_marked`, shell first so it stops forking,
before the best-effort scan. The list is therefore non-empty without any
`/proc` scan. A sleeper forked between `&` and its `echo` is the one gap; it is
bounded by the `sleep <marker>` duration (about 29 s).

(c) `fork_tree` children set `PR_SET_PDEATHSIG(SIGKILL)` then re-check
`getppid()` against the pid captured before the fork (exit if it already
changed). Only async-signal-safe calls. Note: the signal follows the forking
*thread*; the tree is used on the thread that built it. `with_scan` asserts
`root != me()`.

## Tests (each seen failing at runtime first, against a stub or the old code)
- `kill_verified_*`: open fails (EPERM, ENOSYS) refuses and never sends;
  ESRCH on open is Gone; identity runs after open and false sends nothing;
  success sends on the opened fd; pid <= 1 never opens; send ESRCH/EPERM.
- `kill_marked_*` (real processes): a marked sleep dies by SIGKILL; a sleep
  with another marker survives and reports `Mismatch`.
- `drop_kills_the_pid_file_processes_when_every_scan_fails` (b).
- `drop_does_not_kill_a_scanned_non_descendant` (phase 2 parent check) and
  `drop_does_not_kill_a_recycled_recorded_pid` (phase 1 identity).
- `a_killed_harness_leaves_no_fork_tree_behind` (c; re-exec, kills only its
  own child, then expects `P` and `C` to die).
- `with_scan_refuses_the_test_process_as_root`.
- main.rs: `an_unreaped_child_group_is_killed_without_a_reaper_thread`.
Call-site conversions in `command.rs` are mechanical and covered by the
existing re-exec tests plus the helper tests; they are not failing-first.

## Risks
- `pidfd_open` unavailable (old kernel, seccomp): fixtures then refuse to kill
  and may leak sleepers that expire on their own (marker sleeps last 29 s).
- A pid read from a file may be a zombie (empty cmdline): reported `Mismatch`,
  harmless, already dead.
- Marker as extra argv to the libtest re-exec is read as a filter that matches
  nothing under `--exact`; the fixture test still runs.

## Part 2 (not implemented tonight): `ScannerFixture`
One struct whose `Drop` does all cleanup, replacing `Reaped`, `ForkedTree`
(+ injected fork), `MarkedSleeps` (+ injected scan) and the seven hand-rolled
re-exec tests, so correctness no longer depends on each test cleaning up
before its first `assert!`.
- Owns: the scanner `Child` (reaped last, group killed while unreaped), the
  marker, the pid file, an optional tempdir, an `OwnedFd` pidfd per recorded
  pid (opened when the pid is first learned, identity checked at that moment,
  so later kills need no identity re-check).
- Builder: `ScannerFixture::reexec(test_name).mode(..).marker()` spawns the
  test binary with `CHILD`/`MARKER` env and extra argv marker; `.wait_exit(bound)`
  uses `waitid(WNOWAIT)`; `.reported()` parses the newline-terminated file.
- Drop order: pidfd kills of recorded pids, group kill of the scanner while
  unreaped, best-effort scan sweep below the scanner, reap. Never panics.
- Migration: one test at a time; keep each old assertion verbatim; delete
  `Reaped`/`ForkedTree`/`MarkedSleeps` only when no test uses them.
- Risk: a large mechanical diff in 1700-line `command.rs`; do it test by test
  with a mutation check (disable the sweep) for each.
