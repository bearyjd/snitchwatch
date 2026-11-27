---
title: A history clear stages the new store and replaces the old one in one rename
author: Snitchwatch
date: 2026-11-27
branch: fix/history-444-staged-clear
id: l-869e805d
scope: repo
status: active
replaces: l-05a31a45
src: agent
touches:
  - crates/snitchwatch-bridge/src/history/control_clear.rs
  - crates/snitchwatch-bridge/src/history/control_files.rs
---

## Decided
- #444: a clear that records or has rows to keep builds `history.db.new` (carry committed and fsynced) and renames it over `history.db`, because rows held only in memory between the tombstone unlink and the carry insert were lost on a failed insert or a kill. Delete, and a clear with neither, still use rename-to-tombstone.
- The point of no return comes from `replace_store`'s return value, not from the observed step: an error means the old store is in place and gets the generic undo; every `Replaced` outcome is never undone. A rename error is decided by which inode `history.db` now has.
- A directory fsync failure leaves recording off (`ClearedNotDurable`): rows written on top could vanish with a power loss.
- Carried over from #441: a preview counts a leftover tombstone and confirm rechecks it; recovery carries on past a linked leftover; a message never claims recording is off unless that path turned it off.
## Tried and ruled out
- Rename, create, carry, unlink: `recover()` cannot tell an uncommitted carry from a crashed delete.
- A carry sidecar replayed at startup (new format that writes rows at start), an insert retry alone (misses the kill), in-place DELETE (leaves data in freed pages).
## Watch out
- The `O_EXCL` descriptor of the staged file is closed before SQLite opens it: closing it later drops SQLite's POSIX locks.
- Any `history.db` sidecar at rename time must refuse the replace: it would be applied to the new file. The same for staged sidecars.
- Rename and directory-fsync failures are only reachable through the thread-local `Fault` seam. The order file fsync, rename, directory fsync is not provable by kill tests.
- A non-plain entry at a staged name stops `finish_leftover` before it removes anything (a tombstone stays too) until it is removed by hand. `recover` still runs the journal rollback past it, as it does past a linked leftover; any new refusal in `finish_leftover` must keep that.
In the code: history/control_replace.rs, control_clear.rs (stage, after_replace); issue #444.
