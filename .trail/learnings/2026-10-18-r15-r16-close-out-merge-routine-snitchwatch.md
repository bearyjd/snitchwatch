---
title: "r15/r16 close-out: merge routine, scope rule, what stays external"
author: Snitchwatch
date: 2026-10-18
branch: main
cost: { minutes: 27, tokens: 173845 }
touches:
  - crates/snitchwatch-bridge/tests/packaging_shape.rs
  - crates/snitchwatch-bridge/tests/packaging_shape/daemon_and_flatpak_configs.rs
  - crates/snitchwatch-bridge/tests/packaging_shape/fetch_rule.rs
  - crates/snitchwatch-bridge/tests/packaging_shape/main.rs
  - crates/snitchwatch-bridge/tests/packaging_shape/systemd_units_and_docs.rs
  - crates/snitchwatch-bridge/tests/release_artifact.rs
  - crates/snitchwatch-bridge/tests/release_artifact/main.rs
  - crates/snitchwatch-bridge/tests/release_artifact/pack_layout.rs
  - crates/snitchwatch-bridge/tests/release_artifact/tar_rejections.rs
  - crates/snitchwatch-bridge/tests/release_artifact/verify_contract.rs
---

## Decided
- Merge routine: push, PR, independent review (security-reviewer for anything touching files/auth), fix rounds go back to the executor, 6/6 CI (check, test, kirigami, audit, candidate-contract, package-check), merge main into the branch (never rebase), `gh pr merge N --squash --match-head-commit <sha>`, then watch main CI.
- Owner scope rule: r15 = whatever is on main when r14 is promoted. After the cut, packaging/daemon config/units/sockets go to r16 unless they fix the r15 socket change. Tower overrides ProcMonitorMethod to proc after stage.sh; packaged configs keep ebpf.
- The candidate publication names are renamed per round (#147 r14, #177 r15). The owner sets R15_CANDIDATE_REVIEWED_SHA and pushes r15-candidate; we never push it.
- Items needing opensnitchd or the tower (no rules-reloaded push, silent over-budget lists, recommended entries needing captures) live in tracker #178; #35 stays open until the tower confirms the root-only socket.
- After each merge to main, send the tower the merged SHA.

## Tried and ruled out
- Squash merges defeat `git branch --merged`; verifying cleanup needed an ancestor check, `git cherry`, or a diff of the branch's files against main. 47 old local branches could not be proven merged and were kept.
- #110 flakes: closed after 20/20 green CI runs, but two executor runs each saw one unidentified Kirigami failure that passed on rerun. Reopen if CI flakes.

## Watch out
- CI jobs run as root in a fedora container; tests must not assume a non-root user (use the foreign_owned_dir() helper).
- Test clock seam in cache/rules_sync.rs is thread-local (current-thread runtimes only) and does not cover rules_count's awaiting_adoption.
- Settings dir that is a symlink is followed one hop only and must be ours and already 0700; stow users with a 0755 package dir must chmod 700 it (release-note line).
- GitHub API calls sometimes hang for minutes; wrap with `timeout` and retry rather than assuming CI failed.
- `trail init --no-agents` was run on 2026-10-18; it wrote .trail/, .claude/settings.json and 3 files in ~/.claude/. It was committed in PR #183.
