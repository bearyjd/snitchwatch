---
title: Keep both history rows when the daemon cannot prove socket identity
author: Snitchwatch
date: 2026-11-23
branch: main
id: l-f57f8aaf
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge-cli/src/analysis_export/service_tests.rs
  - crates/snitchwatch-bridge-cli/src/lib_tests.rs
  - crates/snitchwatch-bridge-cli/tests/history_capture.rs
  - crates/snitchwatch-bridge/src/analysis_export/build_tests.rs
  - crates/snitchwatch-bridge/src/analysis_export/snapshot.rs
  - crates/snitchwatch-bridge/src/analysis_export/snapshot_tests.rs
  - crates/snitchwatch-bridge/src/cache/connections.rs
  - crates/snitchwatch-bridge/src/cache/traffic_tracker.rs
  - crates/snitchwatch-bridge/src/daemon_watchdog.rs
  - crates/snitchwatch-bridge/src/deferred_answers.rs
---

## Decided
Retain prompt and ping records and flag plausible pairs as possible duplicates, including one-to-one pairs with complete TCP endpoint fields. The existing daemon reports neither an identifier for the socket lifetime nor its network namespace. A tuple can be reused during the settlement window. An equal process path and rule hash cannot disambiguate reuse.

## Tried and ruled out
Matching a single TCP candidate by both endpoints, process path, action and rule identity can silently collapse two sockets. Deferring the match or recalculating cardinality handles late candidates but cannot establish socket identity.

## Watch out
Use the compatibility predicate for candidate flags only. Different known source addresses exclude a pair. Missing source metadata and coalesced ping counts can still leave a plausible pair. Counts that could include both rows need the duplicate disclaimer when the history reader lands.

In the code:
The indexed SQLite history_events view in crates/snitchwatch-bridge/src/history/correlation.rs exposes possible_duplicate. That module tests the single complete tuple, late and ambiguous candidates, missing fields, and distinct source addresses.
