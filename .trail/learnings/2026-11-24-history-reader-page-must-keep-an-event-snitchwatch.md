---
title: "History reader: a page must always move the cursor, and a green stress test must count only delivered pages"
author: Snitchwatch
date: 2026-11-24
branch: feat/history-pr9-reader
id: l-pr9-progress
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/reader_page.rs
  - crates/snitchwatch-bridge/src/history/reader_registry.rs
---

## Decided
- The shrink that fits a page may not settle on zero events while events were loaded: keep the newest one and trim the ledger rows (partial = gaps). A page with `End::More` and a repeated or missing cursor is a stall; the fixture's `all_pages()` now fails on it.
- `Limits::busy_wait` is applied on the read connection (after open, before the schema check). Reads wait out a writer's commit for 20 ms, then say unavailable-retry.
- Control operations use `ReaderRegistry::quiesce` (close, cancel, wait as one step). Overlapping guards count; a timed-out quiesce stays closed until `NotIdle::give_up`.
- A response page that has been taken for sending keeps its slot until dropped; only unsent pages are reclaimed at the deadline.
- A lone half of a broken outage pair counts `max(1, count-1)` in `lost_at_least`. Summaries are per window and must not be summed across pages.

## Watch out
- Cancel has two redundant stops (interrupt handle and the progress handler's flag check). Deleting one passes every test; only deleting both fails. That is by design.
- Stress readers must count `Outcome::Page` only; counting every call made the lock tests pass with a reader that could not open the file.
