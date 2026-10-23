---
title: "Resume after learning mode L1-L5: STATE.md header is stale"
author: Snitchwatch
date: 2026-10-23
branch: main
cost: { minutes: 1, tokens: 61536 }
touches:
  - ".omc/STATE.md"
  - HANDOFF.md
---

## Decided
Resume = read `.omc/STATE.md` (60 lines) plus HANDOFF.md lines 11-44 only, then confirm with `git fetch`, `gh pr list`, `gh issue list`, `gh run list --branch main`. As of 2026-10-23 main is 2a5672d, no open PRs, CI green, learning mode L1-L5 merged. Next work (0b, 0e, r16 eBPF) needs the owner's go-ahead; 0c must not be merged.

## Tried and ruled out
Reading HANDOFF.md whole: unnecessary, the current section is at the top.

## Watch out
The first line under "Where main is" in STATE.md still says d0e0bfa; the real head is 2a5672d. Trust git, not that line.
