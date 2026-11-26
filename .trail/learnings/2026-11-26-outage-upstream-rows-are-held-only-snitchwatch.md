---
title: Outage upstream rows are held only while their own disconnect row is still pending
author: Snitchwatch
date: 2026-11-26
branch: fix/history-432-gap-rows-one-txn
id: l-b652d469
scope: repo
status: active
src: agent
---

## Decided
- The two upstream-loss halves of an outage wait only while that same outage's disconnect row is unwritten. Outages are numbered so this is tracked, not inferred. This narrows the earlier rule that held them whenever any disconnect tally had no end.
- Accepted limit, pinned by a test: an unrelated loss already merged into the same upstream counter as a held half loses its count on a crash. The time span stays covered.
## Tried and ruled out
- Holding every upstream row while a disconnect is open: it stranded unrelated and already-paired losses, and a crash then lost a known count.
- Atomicity tests that only bound the loss total, or only count rows at the end: they also pass when the rows are written in two transactions or one half escapes early.
## Watch out
- Atomicity tests must require none or all rows at every commit and fail each commit in turn; check them against a split-commit mutation.
In the code: history/gaps_flush.rs (halves_await_daemon, flush_group), history/gaps.rs (halves_outage, outages_written), history/gaps_group_tests.rs; issue #432.
