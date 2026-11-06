---
title: "Selectable-text + rounded restyle rollout, and the r16 candidate cut: what bit us"
author: Snitchwatch
date: 2026-11-01
branch: main
cost: { minutes: 159, tokens: 1409338 }
touches:
  - dist/q01-complete-build-2/rebuild-local.py
---

## Decided
- Merge selectable-text and restyle PRs when CI is green, behind my own gate: codex read-only review clean on the exact head, 6/6 CI, mergeStateStatus CLEAN, `gh pr merge --match-head-commit <full sha>`, never force-push (merge main into the branch). After a clean textual auto-merge that touches a file both sides changed, run targeted tests: two sides each added a `contentItem` to GuardedToolButton and git merged it cleanly into a broken file.
- GUI release cuts: the tower session sets R<N>_CANDIDATE_REVIEWED_SHA, points r<N>-candidate at the sha, and promotes; it acts only on the owner's go typed in ITS session, not on a peer relay. Cut only after main CI is green on the exact sha, and after the owner checked the GUI on Plasma: a published candidate cannot be replaced under the same name.
## Tried and ruled out
- `gh pr checks --watch` as a "wait for CI" step: it returns immediately when checks are only QUEUED; poll until all checks are COMPLETED instead.
- `gh run rerun --failed` to recover a failed candidate publish: the packet/provenance pins GITHUB_RUN_ATTEMPT, so it fails with 'run_attempt mismatch'; a FULL rerun works once the branch CI run is complete.
- Open-ended polling loops matching processes by command line: they self-match and run for hours; the tower and owner complained about the churn.
## Watch out
- A Button restyled in place with a custom `background:` and no `contentItem` renders a BLANK label under KDE's native style (org.kde.desktop draws the label inside the background); Basic-style headless tests cannot see it. A source guard now enforces it.
- Pushing the candidate branch also triggers ci.yml (the branch is in its push list); that run takes about 19 min vs the 10 min build, so the publish guard can see an in-flight run. Fix in PR #309: skip only an unfinished FIRST attempt; reruns hold publication.
- The primary checkout's local main is stale: read files with `git show origin/main:<path>`. Codex review output can be 'unable to review' when the codex package is being updated: it prints no findings and no 'clean', so a gate must require an explicit clean line.
