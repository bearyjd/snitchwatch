---
title: Send the tower the merged SHA after each merge
author: Snitchwatch
date: 2026-10-18
standing: true
---

## Decided
A separate `bazzite-tower` session builds the images and runs the VM gate. After merges to main, give it the new main commit SHA, or one batched commit for the release, plus a list of what to check.
Ask the user first if they haven't said to send it. They usually say yes.
The tower session pins by hash. When main moves, send a re-pin note so it doesn't build from a stale head.

_Told to Claude in 3 past sessions here, most recently on Oct 13._
