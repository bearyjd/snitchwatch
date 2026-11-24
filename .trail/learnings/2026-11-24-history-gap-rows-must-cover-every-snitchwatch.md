---
title: "History gap rows must cover every instant: floors and clamps that shrink a gap leave silent holes"
author: Snitchwatch
date: 2026-11-24
branch: feat/history-pr7-gap-causes
id: l-b0e47e66
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/gaps.rs
  - docs/superpowers/plans/2026-11-24-history-pr7-gap-causes.md
---

## Decided
- A gap row may overlap another gap, but must never leave an instant uncovered. Double coverage is acceptable; a hole is not, because the history view would show a silent empty interval.
- After a daemon outage the first ping's upstream loss is written as two rows: the stretch from the last ping to the down edge, and the stretch from the daemon coming back to that ping. No upstream row spans the outage interior (that is the daemon-disconnected gap).
- A back-step clock jump keeps its full span while the overshoot past now is at most 60 s; a larger overshoot is clamped to now. Flapping clock-jump rows merge within 10 minutes.
- The fork/stock daemon capability is an in-memory "default-action marker seen this session" flag, never persisted.

## Tried and ruled out
- Clamping a clock-jump row's end to now: it shrank a 10 s ordering-unreliable span to 0.5 s, and the test was rewritten to match instead of failing. Review caught it.
- Starting the post-outage upstream row at the daemon-up time only: it removed the overlap but left the stretch before the outage uncovered. The first test only asserted the start, not coverage.
- Tests written after the code, with no failing run: three review rounds found holes the original crash tests could not see. Assert coverage over the ledger, and watch each test fail first.

## Watch out
- The gaps table requires count >= 1, so a single lost event spanning an outage is written as two rows of count 1 and overcounts by one on an already-uncertain row. Fixing it means relaxing that CHECK.
- A loss reported after the capture closes is dropped on purpose, to keep the clean session end.
- Daemon-disconnect tests use the watchdog with a real Capture, not MockOpensnitchd.
- /tmp hit a disk quota during cargo runs; use a target dir and TMPDIR under the home directory.
In the code: crates/snitchwatch-bridge/src/history/gaps.rs (ClockWatch, Losses, upstream split), ledger.rs (merge_into), docs/superpowers/plans/2026-11-24-history-pr7-gap-causes.md.
