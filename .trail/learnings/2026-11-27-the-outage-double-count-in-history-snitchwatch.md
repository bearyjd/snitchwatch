---
title: The outage double count in history loss totals is an accepted limitation
author: Snitchwatch
date: 2026-11-27
branch: docs/history-432-known-limitation
id: l-dc53e53c
scope: repo
status: active
replaces: l-0a23df73
src: agent
---

## Decided
- The owner accepted that loss totals can read high around an outage, because every fix tried was worse. Views must show them as estimates. If it causes problems, how outage losses are stored (the database schema) may be refactored later.
## Tried and ruled out
- Committing the rows together in the writer: it cannot tell which merged tally belongs to which outage, and releasing several pins at once can leave a coverage gap after a crash.
- Holding rows back until the disconnect row could be written: a crash then lost known counts.
- Pairing in the reader using the durable watermark: failed writes merge both halves into one row, a crash moves the watermark past the stretch, and a pending prompt makes genuine separate losses get halved.
## Watch out
- Do not reopen this with another writer or reader heuristic; probe any new idea against all of these cases, or plan a schema change.
In the code: history/gap_counts.rs module doc; issue #432, closed PR #443.
