# Sweep SIGSTOP phase: considered and rejected (2026-11-06)

Scope: `crates/scanner-privileged` (`containment.rs`, `command.rs`).

## Problem
A descendant forking in a tight loop in its own session can fork between the
sweep's scan and its kill. Those children are not in the scan, and if their
parent dies first they are orphaned. Fable's LOW: they could survive both the
2 s sweep bound and `killpg(own_group)`. A second, minor point: the
`own_group <= 1` early return in `expire_watchdog_with` is unreachable in
production but is the only way to end Expired with the scanner alive.

## Considered: a SIGSTOP phase
Per pass, stop every live descendant (pidfd / parent-checked), re-scan once for
late children, then SIGKILL. A stopped process cannot fork.

## Why it was rejected (Codex review findings)
1. Unwind cleanup: a panic mid-pass strands already-stopped pids; they stay
   stopped, out of reach of the own-group kill when in another session.
2. Bare-pid stop fallback: without a pidfd the stop goes by pid after a parent
   check, so a recycled pid can be frozen.
3. Unbounded kill pass: killing every stopped pid regardless of the deadline
   can overrun main's 5 s exit wait.
4. A failed kill after a successful stop is never reported: `killpg` kills the
   scanner before the sweep's diagnostics are logged.
5. With the scanner as a subreaper (as in production) the flat-out fork loop
   dies without a stop phase: orphans stay in the subtree and the next pass
   finds them. The end-to-end test passed with the stop phase disabled.

The stop phase added more failure modes than it removed, so the sweep is
unchanged.

## Kept
- `debug_assert!(own_group > 1 || cfg!(test))` in `expire_watchdog`
  documenting the invariant (the runtime guard and its test are unchanged).
- Fixture hygiene: the fork-loop fixture uses a per-run unique marker (pid,
  clock, counter), counts only `sleep <marker>` processes below its own root,
  cleans up only its own subtree, and treats a failed or truncated observation
  as an error, never as "none". The fork-loop test stays as a regression guard
  of the existing sweep. Tests: `markers_are_unique_per_call`,
  `a_foreign_process_with_the_same_argv_is_neither_counted_nor_killed`,
  `a_failed_observation_is_an_error_not_an_empty_list`.
