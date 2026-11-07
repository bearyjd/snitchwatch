# Watchdog sweep: SIGSTOP phase (2026-11-06)

Scope: `crates/scanner-privileged` (`containment.rs`, `command.rs`) only.

## Problem
1. `kill_descendants_with` scans `/proc`, then SIGKILLs the listed pids one by
   one. A descendant forking in a tight loop (own session) forks again between
   the scan and its own kill; those children are not in the list, get orphaned
   when it dies, and survive both the sweep bound and `killpg(own_group)`.
2. `expire_watchdog_with`'s `own_group <= 1` early return is unreachable in
   production (pid comes from `getpid`) yet is the only path to Expired alive;
   the invariant is undocumented.

## Approach
1. Per pass with live descendants: SIGSTOP each (same pidfd + parent-checked
   path as the kill, so a recycled pid is never signalled), re-scan once to
   catch children forked before the stop took effect (stop those too), then
   SIGKILL the union. A stopped process cannot fork; SIGKILL works on stopped
   processes. A stop failure other than ESRCH is counted but never aborts the
   kill. Settled invariants unchanged: sweep before `killpg`, nothing logs or
   blocks in between, one deadline checked per `/proc` entry and per signalled
   pid (stops and kills), failed/truncated scans never count as empty, two
   consecutive empty passes to complete.
   The signal-generic helper is shared; `kill_if_descendant*` keep SIGKILL.
2. `debug_assert!(own_group > 1)` documenting the invariant; the runtime guard
   and its test stay.

Review follow-up: every pid a stop was issued for is recorded and killed on
every exit path (deadline, failed re-scan), newest first, with the kill pass
exempt from the deadline; the deadline is only checked before a stop. The
fork-loop fixture uses a per-invocation marker and only counts and cleans up
processes below its own root.

## Tests (each seen failing first)
- injected: stop precedes kill, re-scan catches late children (stopped, killed).
- real: a setsid shell loop forking `sleep` flat out is fully dead after the sweep.
- real: an already SIGSTOPped process is killed.
- real: the stop path never signals a pid outside the tree.
- injected: deadline bounds stops+kills; a failed stop still kills and counts.
- `completion_needs_two_consecutive_empty_passes`: scan count 4 -> 5 (the
  re-scan is one extra scan on a non-empty pass); expectations otherwise intact.

## Risks
Fixture leaks (all children killed and reaped via guards). One word is banned
in this crate's identifiers and comments (bridge `dormant_tests`); the state
type is `WatchdogState`.
