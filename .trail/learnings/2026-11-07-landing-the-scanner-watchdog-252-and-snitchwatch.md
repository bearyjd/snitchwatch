---
title: "Landing the scanner watchdog (#252) and text-trace (#316): codex rounds, guard-name collision, accepted exception"
author: Snitchwatch
date: 2026-11-07
branch: main
cost: { minutes: 28, tokens: 304208 }
touches:
  - ".omc/STATE.md"
---

## Decided
- Merge rule held: codex clean on the exact head and 6/6 CI, pinned with --match-head-commit. #252 took 7 codex rounds plus 2 Fable reviews and merged as 01877ed1; #316 merged d9e6a3d6 after 6 rounds with ONE owner-accepted codex MEDIUM (title-delegate Component-URL check spoofable by QML already running in the app; PR comment records it, follow-up is trusted identity or explicit per-page marks).
- Watchdog design that survived review: sweep runs BEFORE killpg(own_group) because that kill destroys the watchdog thread (the scanner is a subreaper so orphans stay findable); one Mutex+Condvar state machine for spawn, expiry and exit; a lock-free EXPIRING flag checked before spawn() and in pre_exec; /proc read errors other than ENOENT/ESRCH fail the scan; the scanner sweeps escapees on a clean exit.
- text-trace is an ALLOWLIST (textTraceable / textTraceTitle marks), not "all on-screen text": redacting editable fields item by item kept leaking through bound Text, delegates and the tray tooltip.

## Tried and ruled out
- Reordering the watchdog to killpg first, then sweep: tried in a scratch edit, helpers survived because the kill takes down the watchdog thread.
- Chasing leak vectors in the unrestricted tracer: each codex round found another path.
- Fixed-time sleeps in watchdog tests: replaced by readiness handshakes.

## Watch out
- history::dormant_tests::primitives_have_no_production_call_sites in snitchwatch-bridge scans EVERY workspace source (including scanner-privileged) for the word "lifecycle". It only shows in the CI `test` job and in `cargo test -p snitchwatch-bridge`, not in `-p scanner-privileged`. Never name an identifier lifecycle outside the history module.
- A worktree's .git is a gitfile, so the release container build cannot run from one; use a real clone with a self-contained submodule (repack and drop alternates).
- `git worktree remove` refuses worktrees with submodules; needs --force.
- A wait loop on `gh pr checks` ends early when the list is momentarily empty right after a push; wait for all required checks to exist and be complete.
- Executors must be told to write the failing test first and show it failing at runtime; two skipped this.
