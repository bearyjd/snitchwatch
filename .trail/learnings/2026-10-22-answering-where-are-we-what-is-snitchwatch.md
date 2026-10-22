---
title: "Answering 'where are we / what is open' quickly"
author: Snitchwatch
date: 2026-10-22
branch: main
cost: { minutes: 1, tokens: 54612 }
touches:
  - HANDOFF.md
---

## Decided
- Open work lives in GitHub, not in the repo docs: `gh pr list` and `gh issue list`. As of 2026-10-21 there were no open PRs and two open issues, #178 (umbrella of external dependencies for the daemon and tower) and #35 (waiting on the tower to confirm the system socket).
- Issue #178's body is the canonical list of what is blocked outside this repo. Read it with `gh issue view 178`.

## Tried and ruled out
- `.omc/STATE.md` does not exist in this repo. `HANDOFF.md` has a stale "current" section dated 2026-10-09, so don't report status from it.

## Watch out
- `git status` normally shows `graft/` untracked, plus modified `.claude/settings.json` and `.trail/hook.cjs`. That is tooling noise, not work to commit.
- After merges, the bazzite-tower session needs the new main SHA, or one batched SHA.
