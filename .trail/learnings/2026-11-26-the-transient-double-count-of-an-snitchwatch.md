---
title: "The transient double count of an outage's lost events resisted both writer-side and reader-side fixes"
author: Snitchwatch
date: 2026-11-26
branch: fix/history-432-reader-unanchored-pair
id: l-0a23df73
scope: repo
status: active
src: agent
---

## Decided
- Nothing is fixed yet. A read taken before the disconnect row is written can overstate the loss, and after a crash in that window the overstatement stays.
## Tried and ruled out
- Committing the rows together in the writer: the writer cannot tell which merged tally belongs to which outage (outages recur, the clock can rewind, one write can fail), and releasing several pins at once can leave a coverage gap after a crash.
- Holding rows back until the disconnect row could be written: a crash then lost known counts.
- Pairing unwritten outages in the reader using the durable watermark: failed writes merge both halves into one row, a crash moves the watermark past the stretch, and a pending prompt holds the watermark back so genuine separate losses would be halved.
## Watch out
- The counting code says the total never exceeds the truth; that is not so in these windows. Probe any new idea against all of these cases first.
In the code: history/gap_counts.rs (summarize), history/gaps.rs, history/reader.rs (read_ledger reads rows and watermark separately); issue #432, closed PR #443.
