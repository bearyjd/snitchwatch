---
title: "Who sets R<N>_CANDIDATE_REVIEWED_SHA and pushes r<N>-candidate"
author: Snitchwatch
date: 2026-10-22
branch: feat/unanswered-prompt-context
cost: { minutes: 1, tokens: 1709 }
touches:
  - HANDOFF.md
  - docs/superpowers/plans/2026-10-21-unanswered-prompt-diagnosability.md
  - crates/snitchwatch-bridge/src/deferred_answers.rs
  - ".omc/STATE.md"
  - docs/superpowers/plans/2026-10-22-log-export-for-analysis.md
  - docs/superpowers/plans/2026-10-22-r16-platform-upgrade.md
  - crates/snitchwatch-bridge-cli/src/relays.rs
---

## Decided
- The tower session sets `R<N>_CANDIDATE_REVIEWED_SHA` on bearyjd/snitchwatch and pushes the reviewed sha to `r<N>-candidate`. Its gh token is bearyjd with admin on the repo; `R13_` and `R14_CANDIDATE_REVIEWED_SHA` already exist, set by the tower side on 2026-10-13. Both steps wait on the owner's explicit OK, which the tower asks for after its own re-pin PR (#116 for r15) is merged.
- We (this repo's session) set nothing and push nothing for candidates. We send the tower the final main sha, with CI green and the combined release-bridge-repro OK, then stay idle on main until the tower reports the push.

## Tried and ruled out
- Supersedes the 2026-10-18 note saying "owner sets the variable": that part is stale. "We never push" still holds.

## Watch out
- The candidate-gui workflow's build/publish jobs only run when the pushed sha equals the variable, so the variable must be set first, to the exact sha pushed.
