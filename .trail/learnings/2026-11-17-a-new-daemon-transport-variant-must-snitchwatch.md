---
title: A new daemon transport variant must be audited against every check that compares for the TCP transport
author: Snitchwatch
date: 2026-11-17
branch: feat/35-tcp-dev-only
cost: { minutes: 28, tokens: 74667 }
id: l-b40fff51
scope: repo
status: active
src: agent
touches:
  - Cargo.lock
  - crates/snitchwatch-bridge-cli/examples/live_rule_change.rs
  - crates/snitchwatch-bridge-cli/src/activation.rs
  - crates/snitchwatch-bridge-cli/src/analysis_export/service_tests.rs
  - crates/snitchwatch-bridge-cli/src/cli.rs
  - crates/snitchwatch-bridge-cli/src/config.rs
  - crates/snitchwatch-bridge-cli/src/leftover_unix_tests.rs
  - crates/snitchwatch-bridge-cli/src/lib.rs
  - crates/snitchwatch-bridge-cli/src/lib_tests.rs
  - crates/snitchwatch-bridge-cli/src/main.rs
---

## Decided
The per-user socket got its own untrusted transport (like TCP), not the root-only Unix one. Trust gates must ask "is this a trusted daemon", never "is this TCP".

## Tried and ruled out
Reusing the root-only Unix transport for the owner-uid socket: a same-uid process could take over commands. Two Claude reviews missed that two gates still compared against TCP only; the codex round caught it.

## Watch out
Codex reads the working tree, so a dirty vendor checkout shows up as a bogus "submodule moved" P1. Check the committed gitlink first.
In the code: peer_is_trusted_daemon(), rules_import.rs, rule_commands/gates.rs.
