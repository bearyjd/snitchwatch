# History #444: make a clear's carried rows durable before anything is removed

Status: CONFIRMED by the owner 2026-11-26, with all three recommendations (see Owner decisions). Security review of the plan done (no CRITICAL/HIGH; 3 MEDIUM and 3 LOW folded in below). No code changed. Base: main `93e7b1e8` (docs-only commits since `55a41edc`; line citations are for the code, unchanged).

## Problem
The rows accepted after the preview ("carry", PR 10 decision 2: keep them) exist only in memory between the rename to the tombstone and the carry commit, and `recover()` unlinks any tombstone it finds (`control_files.rs:277-280`). So a failed carry insert, or a kill at the `Renamed`, `TombstoneRemoved` or `Created` step, loses them. `control_crash_tests.rs:180-191` asserts that loss today (0 events at every step but `Begin` and `Carried`).

## Design: stage, then replace in one rename (clear only)
1. `control_files.rs`: fixed names `history.db.new` plus `-journal`, `-wal`, `-shm`. `check_removable` requires them to be plain files; `finish_leftover` (so also `recover` and delete) always unlinks them, with the same link-count rule.
2. Create the staged file through the directory fd with `O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600, after unlinking any leftover. Never create it by path (`schema::prepare(path)` alone would follow a planted link).
3. Prepare the schema in it, insert the carry, commit with `synchronous = FULL` set explicitly, close, then fsync the staged file.
4. `StoreDir::replace_store`: refuse if any staged sidecar OR any `history.db` sidecar exists (a journal left beside the old store would be applied to the new file and corrupt it; the journal mode is DELETE, `schema.rs:259`). Read the old store's link count, `renameat(history.db.new, history.db)`, fsync the directory, return `Linked` if the count was above 1.
5. `remove_files`: the recheck and `read_carry` stay as they are (`control_clear.rs:220-221`). A clear that was recording or has a carry takes the staged path; delete, and a clear with neither, keep `remove_store`.
6. `remove_and_finish`: a failure before the replace leaves the old store intact (drop the staged file, generic undo, plain refusal is true). A failure at or after it never undoes: retry `spawn_capture` once, then return a new `Refused::ClearedNotResumed` ("history was cleared; N rows accepted since the preview were kept; recording could not be resumed"). `Linked` stays `DataLeft` with recording resumed.
7. `close` still advances the generation on the staged path, so older preview tokens stay stale.

Unchanged: preview token and recheck (the staged file holds only rows above `max_id`), the writer lock, delete (never keeps rows), no schema change.

## Review amendments (override the steps above where they differ)
- **A1, outcome of the replace.** On a `renameat` error, `fstatat` whether `history.db` now has the staged inode and decide from that; the old store in place means a plain refusal. On a directory-fsync failure, return a separate refusal saying the clear may not survive a power loss, and leave recording OFF. Never report "cleared" for either without that check.
- **A2, inode identity.** Close the `O_EXCL` fd before SQLite opens the file (closing it later drops SQLite's POSIX locks, `sqlite_file.rs:116-118`), keeping its `(dev, ino)`. Assert the same `(dev, ino)` after prepare and again before the rename; also 1 link, expected `user_version`, and `events` count equal to the carry length.
- **A3, check timing.** After staging, with the staged connection closed and immediately before `renameat`: re-check no staged or `history.db` sidecar, tombstone absent, `history.db` a plain file.
- **A4.** `has_history()` counts `history.db.new` (`control_files.rs:175`).
- **A5.** New `Step::Staged` and `Step::Replaced`; the `reached` logic stops relying only on `Renamed`/`TombstoneRemoved`. The order file fsync, rename, directory fsync is verified by review or strace, not by kill tests or mutations.
- **A6.** A failed staged fsync discards `.new`; never retry then rename. The old inode is freed by the rename, not scrubbed (no worse than the tombstone path).
- Tests added: rename error and directory-fsync failure outcomes; swapped staged inode refused; test 7 plants the journal after staging.

## Review round 1 (security review, LOW findings; override A1-A6 and the steps where they differ)
- A staged name with another link is a plain files refusal naming a staged copy. "Deleted history is still on disk" is kept for the tombstone and `history.db`.
- `finish_leftover` unlinks the tombstone (and orphan sidecars) and syncs before it refuses a staged name that is not a plain file. A linked tombstone is reported first. This replaces "a staged name that is not plain stops the leftover before anything is removed".
- A clear that finds no `history.db` when it rechecks moves the staged store there with `renameat2(RENAME_NOREPLACE)`, after the same identity, sidecar and tombstone checks and with the same sync order. Anything that has appeared at the name is refused and left. A file system without that flag refuses; there is no fallback. A store that was there at the recheck and is gone at the replace is still refused.
- The staged store's `rules_seen` count must equal the carried rules, as `events` does.
- Kill tests also cover a clear that is not recording, with and without a carry.

## Crash points
| Kill at | On disk | Next start | Result |
|---|---|---|---|
| Before staging | old store | nothing | old store whole |
| Creating or inserting | old store, `.new`, maybe its journal | unlinks them | old store whole |
| Staged, before rename | old store, `.new` | unlinks `.new` | old store whole (clear never acknowledged) |
| Renamed, before directory fsync | process kill: new store; power loss: old or new | unlinks `.new` if present | new store with carry, or old store whole |
| After fsync, before capture starts | new store with carry | normal start | carry kept |

The power-loss rows hold only with steps 3 and 4 (data fsynced before the rename).

## New windows
A leftover staged file (copies only, cleaned up); a hot journal at rename time (refused); extra disk use while staging (fails closed before anything is removed); the link count read before the rename (the same race as #445); a refusal after the replace (new variant).

## Rejected
Rename-create-carry-unlink (recover cannot tell an uncommitted carry from a crashed delete); a carry sidecar replayed at startup (new format, writes rows at startup); insert retry plus a new refusal only (misses the kill); in-place `DELETE` (a clear must not leave data in freed pages).

## Tests (RED first)
1. Kill-at-every-step: after restart the store holds the 4 old events or exactly the 1 carried row. This changes the current 0-event expectation.
2. Disk full while staging: old store whole, still recording, no `.new` left, plain refusal.
3. `spawn_capture` fails after the replace: `ClearedNotResumed`, carry present, old rows gone.
4. Leftover `.new` and `.new-journal`: removed by `recover`, `history.db` untouched.
5. Delete with a leftover `.new`: nothing survives.
6. Symlink or FIFO at `history.db.new`: refused, nothing removed or followed.
7. A leftover `history.db-journal` at replace time: refused, old store intact.
8. Mutations: dropping either sidecar check, or the staged name from `finish_leftover`, fails a test.

## Out of scope
Delete, disable and non-recording clear paths; #441 tombstone preview logic; reader reopen after a replace (PR 11); range delete and retention (PR 10b); schema changes.

## Owner decisions (all accepted as proposed, 2026-11-26)
1. No space to stage: refuse the clear (proposed), or offer "clear without keeping", which departs from decision 2 and needs a new confirmation.
2. Add `Refused::ClearedNotResumed` before PR 11 exposes the gate (proposed).
3. Tighten the crash invariant from "old store whole or empty store" to "old store whole, or new store with the carry"; update the PR 10 plan text and the crash test (proposed).
