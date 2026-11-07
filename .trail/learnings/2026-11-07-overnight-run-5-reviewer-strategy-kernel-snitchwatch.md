---
title: "Overnight run 5: reviewer strategy, kernel cmdline race, executor safety incidents"
author: Snitchwatch
date: 2026-11-07
branch: main
cost: { minutes: 340, tokens: 1324083 }
touches:
  - crates/snitchwatch-kirigami/src/analysis_export_controller/tests.rs
  - crates/snitchwatch-kirigami/tests/application_widgets.rs
  - crates/snitchwatch-kirigami/tests/common/mod.rs
  - crates/snitchwatch-kirigami/tests/harness_isolation_guard.rs
  - docs/superpowers/research/2026-11-06-flake-259.md
  - docs/CODEMAPS/data.md
  - docs/CODEMAPS/dependencies.md
  - docs/CODEMAPS/gui.md
  - docs/CODEMAPS/scanner.md
  - docs/CONTRIBUTING.md
---

## Decided
- All four late PRs (#321 the #259 flake fix, #326 containment cleanup flake + #322 part 1, #324 codemaps/docs, #323 plan) merged with 6/6 CI and a clean independent Fable/Opus review on the exact head, pinned. Fable/Opus-high are the deciding reviewers; codex stays as extra input (it found real issues but kept finding new ones every round).
- For generated docs use ONE exhaustive sentence-by-sentence sweep (Fable) before any sampling reviewer: codex sampling found 5-8 new false statements per round for three rounds; the exhaustive sweep produced the whole remaining list in one pass and the next confirmation was CLEAN.
- #259 root cause: a lib test loads the production main.qml, whose refreshPreferences attached the real learned file to the process-global recorder. Fix is a `|| cfg!(test)` demo branch plus TEST_LOCK plus a pre-main (.init_array) isolation constructor in tests/common/mod.rs and a source-scan guard test.
- Containment CI flake root cause: right after exec a process shows an EMPTY (or its parent's) /proc/<pid>/cmdline until the new mm's arg_start/arg_end are set, so a cmdline identity check reads Mismatch and the kill is skipped. Fix: retry Mismatch (50x10 ms, fresh pidfd each time), treat zombie/vanished as Gone, a /proc read error as Refused (never Gone).

## Tried and ruled out
- A SIGSTOP phase in the watchdog sweep: five codex findings, all created by the feature, and the flat-out fork test passes without it (the scanner is a subreaper). Rejected, decision record in docs/superpowers/plans/2026-11-06-sweep-sigstop.md.
- Killing the group first, then sweeping: killpg(own_group) kills the watchdog thread itself.
- Runner image as the cause of the containment flake (the passing runs were on the same ubuntu-26.04 image).

## Watch out
- NEVER run binaries from target/debug/deps directly: one is a hard link of the real app; a reviewer's `--list` launched it under the real HOME.
- An executor really ran kill(-1, SIGKILL) while writing a failing-first test with a live placeholder; fixtures now go through containment::fixture_signal.
- Whole kirigami lib/integration test runs used to rewrite the developer's real ~/.config/snitchwatch/learned.json; test only on a throwaway HOME (set CARGO_HOME and RUSTUP_HOME to the real ones).
- A wait loop on PR checks ends early right after a push because the rollup still shows the previous head's completed checks; wait for a non-completed check first.
- The bridge `history::dormant_tests` guard bans the words lifecycle and pins in any .rs source; it only fails in the CI test job.
- Host load average 70-90 from stress loops made unrelated probes fail; run reproduction loops at moderate load.
