---
title: bridge tests use the per-user Unix gRPC socket through user_unix_for_tests; the TCP keep-set stays; per-user peers share one connection key
author: Snitchwatch
date: 2026-11-19
branch: test/386-user-unix-helper-cli-src
cost: { minutes: 30, tokens: 51784 }
id: l-d6047596
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-bridge-cli/src/analysis_export/service_tests.rs
  - crates/snitchwatch-bridge-cli/src/leftover_unix_tests.rs
  - crates/snitchwatch-bridge-cli/src/lib_tests.rs
  - crates/snitchwatch-bridge-cli/src/pause_tests.rs
  - crates/snitchwatch-bridge-cli/src/rule_commands/gates.rs
  - crates/snitchwatch-bridge-cli/src/rule_hits_restart_tests.rs
  - crates/snitchwatch-bridge-cli/src/rules_count_hint_unix_tests.rs
  - crates/snitchwatch-bridge-cli/src/rules_import/e2e_tests.rs
  - crates/snitchwatch-bridge-cli/src/config.rs
  - crates/snitchwatch-bridge-cli/src/lib.rs
---

Bridge unit tests dial MockOpensnitchd::connect_unix(grpc_endpoint.unix_path()) on a bridge built with BridgeConfig::user_unix_for_tests(tempdir). The helper never touches XDG_RUNTIME_DIR; restart tests give each bridge its own subdir and must create it first (the bridge only makes the socket dirs). Kept on TCP: the ephemeral-port test in lib_tests.rs and the cli_flags/state_dir binary-gate tests. Every per-user Unix peer has ConnKey None, so a test with two concurrent mocks that relies on distinct peers changes meaning. Mutating fan-out to current-stream-only leaves bridge-cli green (single mock); the red tests are in snitchwatch-bridge daemon_commands.
