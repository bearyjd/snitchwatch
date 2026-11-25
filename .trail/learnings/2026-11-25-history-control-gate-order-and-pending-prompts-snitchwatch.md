---
title: "History control gate: quiesce readers before advancing the generation, and a pending prompt's pin leaves only with a gap"
author: Snitchwatch
date: 2026-11-25
branch: feat/history-pr10-control-gate
id: l-pr10-control-gate
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/control.rs
  - crates/snitchwatch-bridge/src/history/control_clear.rs
  - crates/snitchwatch-bridge/src/history/control_files.rs
  - crates/snitchwatch-bridge/src/history/pending.rs
  - docs/superpowers/plans/2026-11-24-history-pr10-control-gate.md
---

## Decided
- Order of every control operation: close read admission and wait for the readers (`ReaderRegistry::quiesce`), then persist the mode (the only refusable step: a refusal after the generation advanced strands every pending prompt, which settles stale and keeps its pin), then advance the generation (only through the quiesced guard), then stop the writer, then touch files. The spec had the first two reversed; the code makes the reverse impossible. Spec edited.
- A prompt's arrival pin lives in a pending registry (`pending.rs`), not in the guard. A settle takes it back out; a stale settle leaves it. At the writer's end (stop, disable) all pending pins become one tally (`write-failed` on a plain stop, `disabled` on disable), so one gap row from the earliest arrival counts the prompts and releases the pins in the same commit. Clear and delete discard them with the old store (owner decision 5).
- Clear is rename-to-tombstone then unlink, never an in-place delete. Every file name is a compiled-in exact name, relative to one checked directory handle (`fstatat`/`renameat`/`unlinkat`); anything not a plain file is refused unremoved. Crash recovery (`recover`) runs at every bridge start, history on or off.
- What was accepted after the preview is kept: a clear reads those rows (id above the token's max id) before the unlink and inserts them into the new store before its writer starts.
- The generation is a random 32-bit epoch in the high bits, so a cursor from another bridge process is refused.
- `flock` on a separate `history.lock`, taken by the gate when the history directory exists; the second bridge records nothing.

## Tried and ruled out
- Releasing the pin in `settle` on a mismatch (the old #420 path): invisible to every test that settles after the writer finished, so it survived the first mutation run. Only a test that settles with no writer to drain it catches it.
- Looping a mutation that moved a window index backwards: an endless scan. Mutate the SQL bound instead.

## Watch out
- `RunningBridge` stores the `ControlGate`: it owns the writer flock, and the capture slots handed to the gRPC service and the watchdog do not. A gate kept in a local of `run_with_incoming` drops its lock when the function returns.
- The watchdog tells a capture the daemon is down by the capture it last told (a `Weak`), not by the edge alone: a capture swapped in mid-outage never saw the edge.
- A refusal after the writer stopped that cannot restart recording is `Refused::RollbackFailed`, never the plain refusal.
- The recheck at confirm runs without the preview's statement budget; only the advisory preview scan is cut at 100 ms.
- A disable then re-enable writes two `disabled` rows: the gate's one gap for the pending prompts, and the ordinary disabled interval the next session records.
- Settings temp files have random names and are not removed by delete (they hold only settings).
- `capture.rs` is near its size limit; new capture-facing code goes in `capture_control.rs`.
- A dormant-test word list counts the bare word `history` in a hook file: name a variable `gate` or `recorder`, not `history`.
In the code: crates/snitchwatch-bridge/src/history/control*.rs, pending.rs, capture_control.rs.
