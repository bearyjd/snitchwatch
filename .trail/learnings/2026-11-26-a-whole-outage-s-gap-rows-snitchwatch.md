---
title: "A whole outage's gap rows commit together, and nothing is held back to wait"
author: Snitchwatch
date: 2026-11-26
branch: fix/history-432-gap-rows-one-txn
id: l-568cf04e
scope: repo
status: active
replaces: l-b652d469
src: agent
---

## Decided
- Upstream-loss halves and their disconnect row go in one transaction only when all of them are pending and that disconnect row's end is known or the session is closing. Every other tally is written on its own.
## Tried and ruled out
- Holding upstream rows until their disconnect row could be written: tallies merge per slot, so unrelated and already-paired losses were held too, and a crash then lost known counts. It traded a temporary overcount for permanent loss.
## Watch out
- If the daemon drops again before a flush, the loss total reads high until that second outage closes. This is a known, temporary case.
- An unrelated unwritten loss merged into a real group shares that group's write failure.
- Atomicity tests must fail against the old one-row-at-a-time write order.
In the code: history/gaps_flush.rs, history/gaps_group_tests.rs; issues #432, #419.
