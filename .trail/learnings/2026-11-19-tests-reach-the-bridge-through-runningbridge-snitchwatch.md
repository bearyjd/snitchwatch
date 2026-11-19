---
title: Tests reach the bridge through RunningBridge so those hooks stay public; unused receivers and modules were narrowed
author: Snitchwatch
date: 2026-11-19
branch: refactor/385-bridge-cli-pub-surface
cost: { minutes: 21, tokens: 11429 }
id: l-ab63f0d7
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge-cli/src/running.rs
  - crates/snitchwatch-bridge-cli/src/lib.rs
---

In snitchwatch-bridge-cli, bridge-cli/tests and tests/integration use RunningBridge fields (broadcast_tx, inbound_tx, tray_rx) and set_curated_uid_lookup / run_with_answer_timeout, so they stay pub (now doc(hidden) where test-only). Kirigami production code never touches RunningBridge. notice_rx had no users and was removed; rule_hits_storage, profile_storage and storage became private modules (their consts pub(crate)); set_curated_account_lookups and resolve_grpc_listen are pub(crate). Narrowing a module makes its pub items unreachable_pub hits, so narrow those too. Remaining unreachable_pub hits in config.rs (GRPC_BIND_ENV, USER_GRPC_DIR, USER_GRPC_SOCKET) pre-date this and were left.
