---
title: "History reader: seek ties and older instants as two index ranges, and never let a stale open session claim the head"
author: Snitchwatch
date: 2026-11-24
branch: feat/history-pr9-reader
id: l-9c41d7e2
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/reader_sql.rs
  - crates/snitchwatch-bridge/src/history/reader_page.rs
  - crates/snitchwatch-bridge/src/history/reader.rs
  - docs/superpowers/plans/2026-11-24-history-pr9-reader-limits.md
---

## Decided
- A page seeks `(ts_ms, id)` with two statements: `ts_ms = ? AND id < ?` (ties at the cursor's instant), then `ts_ms < ?`. Both are pure ranges on `events_ts` (its implicit rowid is the second key).
- The reader holds no lock between statements: each runs in autocommit and a progress handler cuts it at 100 ms (under the writer's 250 ms busy timeout). Rows read before a cut are kept.
- An open session (`end_ms` NULL) runs to the head only when it is the caller's live session. Any other open session ends at `meta.watermark_ms`; what follows is `unexplained`, never "recorded".
- Events are read first, the ledger after, so an eviction in between shows as a marker, not a hole. Consecutive pages share a boundary instant (next page's high edge is the cursor's `ts_ms`).
- Gap counts are lower bounds; an outage's two upstream-loss rows are paired at read time and count `max(1, c1 + c2 - 1)` (#419).

## Tried and ruled out
- `WHERE (ts_ms, id) < (?1, ?2)`: EXPLAIN showed `events_ts` and no sort, yet 50,000 ties at one `ts_ms` cost 110k VM steps at depth against 9k at the head, because SQLite walks the ties and filters. Only a step-count test caught it; the plan text looked fine.
- Mutating the read to sleep after the last row: the lock was already gone, since SQLite releases the read lock when a statement returns DONE, not at reset. Only a sleep inside the row loop holds it.
- Interrupt-on-attach for a cancel that raced the open: SQLite forgets an interrupt that arrives with no statement running. The cancel flag, checked before each statement and in the progress handler, is what counts.

## Watch out
- A cancel that lands while the file is being validated surfaces as an unavailable store unless the open path maps it back to `Cancelled`; the quiesce test found it.
- `ClockJump` gap rows within ten minutes merge, so a test that wants many overlapping gap rows must rotate causes.
- A zero-length gap is a point of its own in the timeline; a gap or session that only touches the window at one instant has no duration in it and is not listed as a segment.
In the code: crates/snitchwatch-bridge/src/history/reader*.rs, timeline.rs, gap_counts.rs; plan docs/superpowers/plans/2026-11-24-history-pr9-reader-limits.md.
