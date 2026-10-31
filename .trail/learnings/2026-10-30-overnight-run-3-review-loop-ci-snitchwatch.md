---
title: "Overnight run 3: review loop, CI queue, hung tests and the sandbox rebuild limit"
author: Snitchwatch
date: 2026-10-30
branch: main
cost: { minutes: 29, tokens: 643524 }
touches:
  - ".omc/STATE.md"
  - docs/superpowers/plans/2026-10-29-overnight-run-3.md
---

## Decided
Per PR: agent implements -> codex review of the DELTA (</dev/null, filesystem-boundary preamble) -> fix every MEDIUM+ -> merge only when codex clean AND 6/6 CI AND head == reviewed SHA (poll gh pr checks, then gh pr merge --merge). Budget 2-4 codex rounds per PR: reviews found real bugs on almost every one. Stacked PR: merge base, `gh pr edit --base main`, wait for fresh CI. Stale mergeable flag (DIRTY, no checks): merge origin/main into the branch.

## Tried and ruled out
Trusting agents' "tests pass" after a rename or merge: two PRs passed CI alone and broke main together (#277 drawer first-control table vs #278 FocusOrderListView): after parallel merges always check main's own CI. Mutant-check claims were sometimes skipped; ask for them explicitly. Resuming agents past ~280k tokens: start a fresh agent on the existing worktree instead.

## Watch out
A hung test cost 11 h (Qt.quit never returns because main.qml hides to tray): wrap long cargo runs in `timeout` and give every QML probe a watchdog Timer. The agent tool sandbox cannot run flatpak-builder (nested bwrap fails binding /etc/resolv.conf): the GUI rebuild must be run by the owner; the wrapper needs origin/main == the SHA passed, so any docs-only merge to main changes the SHA to use. #262 changed shared bridge state_file::read (not purely dormant): check every "dormant" claim against the diff before telling the tower. CI queue is slow (kirigami job 25+ min): plan 2-3 wait cycles per PR.
