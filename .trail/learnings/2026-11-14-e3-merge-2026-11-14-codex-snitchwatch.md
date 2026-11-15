---
title: "E3 merge 2026-11-14: codex rounds, CI drift, publish lock"
author: Snitchwatch
date: 2026-11-14
branch: main
cost: { minutes: 27, tokens: 221433 }
touches:
  - docs/superpowers/plans/2026-11-14-ebpf-e3-race-fixes.md
  - crates/snitchwatch-bridge-cli/src/lib.rs
  - crates/snitchwatch-kirigami/src/daemon_health_model.rs
  - crates/snitchwatch-bridge/src/cache/connections.rs
---

## Decided
#353 (E3) merged as 31b2fe1b after 4 codex challenge rounds; each round found one more real hole (round 1: watchdog clear vs fresh alert, publish ordering; round 2: two direct DiagnosticsReport senders in bridge-cli lib.rs; round 3: GUI health page kept a past session's monitor line on rapid reconnect; round 4 clean). Fix shape: every DiagnosticsReport goes through DiagnosticsCtx::publish (lock across snapshot+send); watchdog clears only alerts recorded before its sample; GUI tracks which session supplied the line. A merge of main into the branch keeps the codex review valid if `git diff origin/main...OLDHEAD` and `git diff origin/main NEWHEAD` have the same patch-id.
When CI fails for reasons outside the PR diff (new clippy lint on a newer Rust, new RUSTSEC), fix it in a separate PR off main, merge it, then merge main into the feature branch.

## Tried and ruled out
Fixing daemon-session identity in this repo: opensnitchd Subscribe ClientConfig.id is ts.UnixNano() per Subscribe, not per process, so a reconnect cannot be told from a replacement. Accepted as a 3 s grace, issue #356. Tell codex it is accepted or it re-reports it.

## Watch out
git worktree remove refuses worktrees that contain a submodule; a fresh worktree needs `git submodule update --init vendor/opensnitch` before cargo builds. package-check can flake with ETXTBSY in ebpf_modules_shape (#355): rerun the failed job. CI audit already ignores RUSTSEC-2026-0194/0195 (quick-xml); only a new advisory fails it. Local rustc 1.99 cannot reproduce Rust 1.100 clippy lints.
