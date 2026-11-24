---
title: "History age expiry spends a persisted budget that only process uptime refills, never the wall clock"
author: Snitchwatch
date: 2026-11-24
branch: feat/history-pr8-retention-eviction
id: l-3e5deb25
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/expiry_budget.rs
  - docs/superpowers/plans/2026-11-24-history-pr8-retention-eviction.md
---

## Decided
- Age expiry is rate-limited by a persisted token bucket (schema v4, one column on the meta table). It refills over 24 hours of the bridge's own monotonic uptime. The wall clock is never used to refill it, so a clock stepped forward (bad RTC, NTP, a step while the bridge was down) cannot make history expire faster.
- Bucket capacity is the larger of an eighth of stored events and 1.25 times events divided by the retention days, so the 7-day option can keep up with its inflow. Default 30 and 90 days are protected at the eighth.
- The debit is written in the same transaction as the delete, so a crash leaves rows and budget agreeing.

## Tried and ruled out
- A jump-detector skip alone: it skips one pass, then every pass removes a tenth of the rows, so a persistent forward step erases history in hours. Two independent reviews found this.
- A persisted wall-clock window-start column: a stepped clock forges it.
- An in-memory budget: resets on every restart.

## Watch out
- A bridge that is mostly down retains data past its retention window, because only uptime refills the bucket. Suspend time is not counted either. This is documented as a deliberate privacy trade-off.
- A store of 32 events or fewer expires wholly in one pass.
- The history_off CLI tests need short TMPDIR and CARGO_TARGET_DIR set in the same shell command, or the Unix socket path exceeds 108 bytes.
In the code: crates/snitchwatch-bridge/src/history/expiry_budget.rs, retention.rs, schema.rs (migration_steps); plan docs/superpowers/plans/2026-11-24-history-pr8-retention-eviction.md.
