---
title: "Overnight run 6 start: PR A enabled-only source.network gate, mutation check"
author: Snitchwatch
date: 2026-11-08
branch: main
cost: { minutes: 5, tokens: 106775 }
touches:
  - crates/snitchwatch-bridge/src/daemon_commands/version_gate.rs
  - docs/superpowers/plans/2026-11-08-overnight-run-6.md
---

## Decided
- PR A (w3, feat/source-network-gate): version_gate.rs refuses_conditions now applies validate_operator_in only to ENABLED rules (daemon skips Compile() for disabled rules); missing-operator refusal stays for all. New failing-first test a_1_9_daemon_accepts_turning_a_source_network_rule_off_but_not_on. Gated script /var/home/user/.cache/prA2.sh writes prA2.log (last line COMMITTED LOCALLY <sha> or NOT COMMITTED).
- Mutation check: making PolicyContext::for_daemon never refuse fails 3 unit tests (version_gate x2, context x1); reverted, git diff clean.

## Tried and ruled out
- sed-rewriting prA.sh's multi-line commit message: only the first line is replaced, so write a fresh script.

## Watch out
- Run tests via the sw-build.env throwaway HOME; skip the critic in Step 0 unless the plan was edited after 2026-11-08 night.
- PR #340 review agent and prA2.sh were still running at the time of this note.
