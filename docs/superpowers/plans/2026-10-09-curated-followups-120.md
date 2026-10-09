# Curated defaults: refused/unanswered install follow-ups (#120 items 13-17)

**Date:** 2026-10-09
**Source:** issue #120, the re-review of PR #119 (merged as `ac446e2`);
item 13 also covers item 9.
**Baseline:** `main` @ `ac446e2`, branch `fix/followups-120-curated`.
**Size:** S. Bridge only: `curated/manager.rs`, `curated/manager_state.rs`,
`curated/reconcile.rs` (docs), one visibility change in
`cache/rules_refused.rs`, and tests (bridge unit tests, bridge-cli
end-to-end on the mock loader model). No wire, protocol or Kirigami change.

## Background (stock v1.8.0, `vendor/opensnitch/daemon/rule/loader.go`)

- `replaceUserRule` puts an enabled rule into memory once it compiles; the
  `always` rule's `Save` comes after it, and only `Save` can fail then. So
  a refused install may apply although the bridge doesn't list it. #119
  records such a name (`maybe_applied`). If the entry is turned off, one
  `DELETE_RULE` goes out by that name.
- `Delete` removes the rule from memory first, then its file. A name that
  isn't in memory gets `OK` and nothing changes on disk.

## Items and design

### #13 (MEDIUM, first): an unanswered install sets `maybe_applied`

After the 15 s timeout, the daemon may still put the install into memory.
It may answer late with `OK` or `ERROR`, or never answer. Today, turning
the entry off then plans nothing (the rule is absent and the entry off),
and the allow keeps applying.

- `Problem` gains `unanswered: bool`, set only by `command_problem` for
  `CommandError::Timeout`. `StreamClosed` and every `send_problem` path
  (not sent) leave it false. After a closed stream, the reconnect snapshot
  is the daemon's memory.
- `apply`: an install that was refused **or unanswered** records the name.
  The rest of the record lifecycle is unchanged:
  - a delete or install answered `OK`, or a refused delete, drops the
    record;
  - a timed-out or unsent delete keeps it, and the sticky failure holds it
    back.
- **Late replies (`DaemonCommands`, within the 30 s grace).** No new path
  is needed:
  - A late `ERROR` to an enabled `always` `CHANGE_RULE` leaves the cache as
    it was (`refused_effect` → `Unknown`). The record set at the timeout
    stands, so turning the entry off sends one delete.
  - A late `OK` upserts the rule. The listed, unedited path deletes it once
    when the entry is turned off, and that `OK` drops the record.
  - Tests deliver both replies by hand, within the grace period.
- Cost: at most one extra `DELETE_RULE` by name, which is harmless on stock.

### #14: a reconnect drops the records

`maybe_applied` becomes `BTreeMap<String, MaybeApplied>`, where
`MaybeApplied = { generation, file_possible }`, in the style of `Failure`.

- It is stamped with the pass's `generation` (the `apply` argument).
- `reconcile` keeps only the current generation's records (`retain`) under
  the same lock that copies them into `DaemonRules`. `DaemonRules` still
  gets a `BTreeSet` of names, so `plan` and its tests don't change.
- `PassKey` is unchanged: a generation change already runs a pass.

### #15: a refused delete after a refused install with no file

Today: the install is refused because `Save` failed, so the rule is in
memory with no file. The follow-up delete removes it from memory, then
`os.Remove` fails (`ERROR`). The cache marks "file may remain", and the
entry reads `OffFileLeft` until an install succeeds. The Rules page counts
the file in `leftOnDisk` too.

The fix is decided from the command sequence and the bridge's own state,
never from the daemon's text.

- **Setting `file_possible`.**
  - Refused install: `file_possible` = whether the cache's `files_left`
    held the name when the install was answered. The refused enabled
    `always` change leaves the marker as it was, so this is its state
    before the install.
    - No marker: `Save` failed, so this install wrote no file, and no
      refused delete left one.
    - Marker set: the stuck file of tower r12.
  - Unanswered install: `file_possible = true`. It may have written its
    file. Under-warning is the dangerous direction.
  - Within one generation the flag is never downgraded. A refused install
    after an unanswered one keeps `true`.
- **Refused delete that had a record.** The `apply` arm takes the record.
  - If `file_possible` is false, the manager forgets the cache's marker for
    the name and the status is `Off`. `RulesCache::forget_file_left`
    becomes `pub(crate)`.
  - It runs inside the pass's `PublishHold`, and the refused delete marked
    the hold changed. So the single publish when the hold drops carries
    `leftOnDisk` without it.
  - Otherwise the marker stays and the status is `OffFileLeft`, as today.
- A delete with no record (a listed copy) is unchanged.
- The pinned tests that change:
  - the e2e test `off_after_an_install_refused_on_save_deletes_what_it_applied`
    now expects a settled `Off`, `leftOnDisk` 0, and no file in the model;
  - the worker test `off_after_a_refused_install_deletes_it_once` now
    expects `Off`.

### #16: debug line

The arm at `manager.rs` ~540 discards an **install** error for a choice
already undone (the entry was turned off while the install was on its
way). That is the arm the issue means. It gets
`debug!(entry = %id, problem = problem.text, …)`. Both are fixed
text/ids, so no token and no daemon text are logged.

### #17: docs and `remove()`

- Fix these doc comments:
  - `CuratedAction::Delete`: the rule can also be a maybe-applied name,
    listed or not, or a rule no longer offered;
  - `DaemonRules::maybe_applied`: an answered install drops it too, as do
    a timeout and a reconnect;
  - `State::maybe_applied`;
  - the `reconcile.rs` module doc: "refused **or unanswered**", and the
    `Off` reading after a refused delete with no file.
- `remove()` (the Remove button for an edited copy) drops the record when
  its delete is answered (`OK` or refused), the same as `apply`. The
  copy's name is then out of memory, so a later turn-off sends no second
  delete.

## Tests (each RED first)

Bridge, `curated/manager_unanswered_tests.rs` (new), against the
scripted-daemon harness. The tests use paused tokio time and direct
`reconcile()` calls, so the 15 s timeout auto-advances. The harness gains
`Daemon::SilentInstalls` (no answer to `CHANGE_RULE`; deletes `OK`).

1. An unanswered install, then off: exactly one `DELETE_RULE`. Forced
   passes before and after it send nothing more. (#13)
2. Late `ERROR` and late `OK` after the timeout, within grace: then off
   sends exactly one `DELETE_RULE`, and forced passes send nothing more.
   (#13)
3. An install cut off by a closed stream records nothing, checked on the
   state directly, because #14 would hide it from planning. (#13)
4. A refused install, then a reconnect whose snapshot lacks the rule, then
   off: nothing is sent and the entry reads `Off`. (#14)
5. An unanswered install, then a refused delete: a settled `OffFileLeft`,
   with the marker kept. (#15, the timeout keeps the warning)
6. An unanswered install, then on again and refused, then off and a refused
   delete: `OffFileLeft`. (#15, the flag is never downgraded)
7. An edited copy with a record, its removal answered `OK`, then forced
   passes: no second delete. (#17)
8. Updated: the worker test `off_after_a_refused_install_deletes_it_once`
   expects a settled `Off` with no marker. (#15)
9. End to end on the mock loader model:
   - the save-failure test is updated (`Off`, `leftOnDisk` 0, no file);
   - the stuck-file test gains a settled `OffFileLeft` and `leftOnDisk` 1,
     so a marker forgotten wrongly is caught. (#15)

**Mutants, each to be killed:**
- the timeout never sets the record;
- `StreamClosed` sets it;
- no generation `retain`;
- the marker is never forgotten;
- `file_possible` is ignored, which always forgets the marker;
- a timeout is recorded as no-file;
- the flag is downgraded;
- `remove()` never clears the record.

The #119 M1 mutants on the restructured record update are rerun too.

## Risks

- **No hot loop, as in #105 and #119.** Every new record leads to at most
  one delete, and the sticky failure holds back a timed-out delete. All of
  these stay intact:
  - the request counter in `PassKey`;
  - the generation stop;
  - no retry until a change;
  - publish holds;
  - the busy re-queue.
  Tests 1, 2 and 7 force extra passes and assert the exact command list.
- **Under-warning.** With no marker known, a refused install's
  follow-up delete reads `Off`. This holds even for a stuck file left before this
  bridge run, since the marker isn't persisted. The daemon restart heals
  it: it lists the rule, and reconcile deletes it then. #119 already
  accepted the same limit after a bridge restart.
- **A brief window.** Between the cache marking the refused follow-up
  delete and the manager forgetting it, a `RequestSnapshot` could show
  `leftOnDisk` 1. The held publish corrects it at once.
- Item #10 (the marker stays after the immutable bit is cleared) and the
  other #120 items are out of scope.
