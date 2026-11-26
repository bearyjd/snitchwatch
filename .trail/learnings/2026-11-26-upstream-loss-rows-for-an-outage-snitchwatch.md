---
title: Upstream-loss rows for an outage are committed together with its disconnect row
author: Snitchwatch
date: 2026-11-26
branch: fix/history-432-gap-rows-one-txn
id: l-022102d8
scope: repo
status: active
src: agent
---

## Decided
- The pre-outage, post-outage and disconnect rows go in one transaction, because a read between separate commits counted one lost event as two.
- While the disconnect tally has no end (a second outage), the upstream rows wait with it. The delay is deliberate; their pins hold the watermark.
- No schema change for the count-of-one overcount: the reader's pair detection already reads one loss as one.
## Tried and ruled out
- Grouping only what is due at flush time: a daemon dropping again before the flush still committed two unpaired rows.
- Allowing a zero count (schema bump and migration): duplicates pair detection.
## Watch out
- "All or none" is per flush, not per outage: a flush between the ping starting and finishing writes the disconnect row alone. One bad pin blocks the whole group.
- A shared cargo target dir across two source paths gave a stale false failure.
In the code: history/gaps.rs (flush_due, flush_group), history/durable.rs (flush_counters_at); issues #432, #419.
