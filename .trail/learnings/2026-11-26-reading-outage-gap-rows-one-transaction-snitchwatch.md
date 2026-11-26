---
title: "Reading outage gap rows: one transaction per complete outage, with one temporary overcount left"
author: Snitchwatch
date: 2026-11-26
branch: fix/history-432-gap-rows-one-txn
id: l-9ed35aba
scope: repo
status: active
replaces: l-022102d8
src: agent
---

## Decided
- A complete outage's upstream-loss and disconnect rows commit together, so a read between commits cannot count one lost event twice for that outage.
- No schema change for the count-of-one split: the reader's pair detection reads it as one.
## Tried and ruled out
- Allowing a zero count (schema bump and migration): duplicates pair detection.
## Watch out
- "All or none" is per flush: a flush between a ping starting and finishing writes the disconnect row alone.
- A second outage before the flush still leaves a temporary overcount.
- A shared cargo target dir across two source paths gave a stale false failure.
In the code: history/gaps_flush.rs, history/durable.rs (flush_counters_at); issues #432, #419.
