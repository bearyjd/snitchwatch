# Refused rule deletes: an honest rules cache

**Date:** 2026-10-08
**Found by:** bazzite-tower's r12 VM run (reproduced twice).
**Baseline:** `main` @ `12c900c` (`9f5e2d6` plus a docs-only HANDOFF
commit; branch `fix/refused-delete-honesty`).
**Size:** M. Bridge (rules cache, `daemon_commands`, curated defaults,
blocklist sink), bridge-cli (rule commands), `mock_opensnitchd` (a loader
model), Kirigami (two plain-text strings). One additive wire field, one new
curated status.

## The bug

1. A recommended rule (#105, `snitchwatch-default-flatpak-flathub`) was
   installed. Its file was made immutable (`chattr +i`), then the entry was
   turned off. The daemon logged "Error deleting rule … operation not
   permitted" and answered the `DELETE_RULE` with `ERROR`.
2. The attribute was cleared, the entry turned on, then off again. The entry
   read "off" and the rule left `SetRules`, but its **file stayed on disk**.
   The bridge logged "deleted a recommended rule"; the daemon logged no
   "Rule deleted".
3. It healed at the next daemon restart: the file reloaded, and curated
   reconcile deleted it.

## What the daemon does (stock v1.8.0)

`handleActionDeleteRule` (`ui/notifications.go`) calls `Loader.Delete` per
rule and answers with the **last** rule's error. `Loader.Delete`
(`rule/loader.go`):

```
rule := l.rules[ruleName]; if rule == nil { return nil }
l.cleanListsRule(rule); delete(l.rules, ruleName); l.sortRules()
if rule.Duration != Always { return nil }
return l.deleteRuleFromDisk(ruleName)   // os.Remove
```

So:
- **An `ERROR` to a `DELETE_RULE` means the rule already left the daemon's
  memory** and no longer applies. Only removing its file failed. The file
  may remain, and the daemon loads it again at its next start. (`ENOENT`
  is an `ERROR` too, for an `always` rule whose file was already gone. The
  bridge can't tell the two apart without parsing the text, so both get the
  same "file may remain" marker. That is a harmless over-warning.)
- A later `DELETE_RULE` of the same name returns `nil` (`OK`) **without
  touching the file**, because the name isn't in memory.
- Temporary rules (`Duration != Always`) never refuse a delete: they have no
  file.
- The bridge sends one rule per `DELETE_RULE` (rule commands, curated,
  blocklist, profile). A multi-rule delete could hide an earlier rule's
  failure behind a later `OK`; none is sent.

The bazzite-tower fork (its PRs #89 and #91) changes none of this.

### Why the bug happened

- The rules cache kept listing the rule after the refusal. So "turn on" saw
  it as installed at once, and sent nothing.
- The later "off" sent a delete. The daemon answered `OK` without removing
  the file.

## What changes

### 1. The cache follows refusals (`cache/rules.rs`, `daemon_commands.rs`)

- `DaemonCommands::on_reply` handles an `ERROR`, as it handles an `OK`, for
  both a waiter and a late reply. The late reply counts only within the
  grace period and on the right stream. It calls a new
  `RulesSync::apply_refused`, which mirrors `apply_confirmed`:
  - it forgets the hit counts of the deleted names;
  - it publishes, or under a `PublishHold` it marks the change held. The
    curated pass runs under a hold, so without this the removal would never
    reach a GUI.
- The cache change happens before the waiter is told, so every caller sees
  the honest list.
- One classification, `RefusedEffect` (in `daemon_commands`), keyed on the
  **command's shape**, never on the daemon's text. Elsewhere that text is
  display-only, and a fork or a forged TCP stream could change it:

| Command | What `ERROR` means on stock | Cache |
|---|---|---|
| `DELETE_RULE` | The rule left memory; its file may remain | Remove it; set the marker |
| `CHANGE_RULE`, disabled + `always` | Compile is skipped for a disabled rule, so only `Save` can fail. The rule as sent is in memory, and its file wasn't written | Upsert it, restamped, as on `OK`. The marker is **not** cleared |
| `CHANGE_RULE`, disabled + temporary | Can't happen on stock: no compile, no save, no timer | Unchanged |
| `CHANGE_RULE`, enabled + temporary | A compile error (the timer error can't happen with editor-checked durations) | Unchanged. `rule_commands/edit.rs` already restores an `always` rule that lost its file first |
| `CHANGE_RULE`, enabled + `always` | **Undeterminable**; see "Open question" | Unchanged |

- **The "file may remain" marker** is a bounded set of names in
  `RulesCache`: at most `MAX_FILES_LEFT` (256). Past that, new names aren't
  recorded and a warning is logged.
  - Set by a refused `DELETE_RULE`, whether or not the name was listed.
  - Cleared when a committed snapshot lists the name again (`left_out`
    names count): the file reloaded.
  - Cleared when a confirmed `CHANGE_RULE` of the name with duration
    `always` succeeds: the daemon rewrote the file.
  - **Deviation from the task text:** a confirmed `DELETE_RULE` does *not*
    clear it. On stock, that `OK` is the no-op `Delete` of a name not in
    memory; the file is untouched. By the time a name is back in memory
    with a file, a snapshot or an `always` install has already cleared the
    marker.
  - A temporary `CHANGE_RULE` doesn't clear it either: `replaceUserRule`
    removes an old file only when the old rule is in memory.
  - Kept across a withdrawal, and across a snapshot that lacks the name: a
    redial without a daemon restart still has the file on disk. It lives
    for the bridge run only.
  - Any change to it bumps the cache revision, so the curated pass looks
    again.
- **Reported:** `RulesNotShown` gains `leftOnDisk` (a count). It is additive,
  `serde(default)`, and omitted when 0. Kirigami's Rules page adds one plain
  sentence to its existing PlainText "not shown" label.

### 2. Curated / recommended rules (`curated/*`)

- `DaemonRules` carries the marker set, as it carries `left_out`.
- `plan_entry`'s absent-rule branch is already right once the cache is
  honest. The install record is dropped, so turning the entry on again
  plans an `Install`: a `CHANGE_RULE` that rewrites the file and reloads it.
- New status **`OffFileLeft`**: absent, turned off, marker set. Kirigami's
  wording: "Turned off, but the firewall service couldn't remove the rule's
  saved file, so it may come back when the service restarts. If it comes
  back unchanged, Snitchwatch removes it then."
  - Older GUIs read it as `Unknown` (`serde(other)`).
- **The no-hot-loop property (#105) holds.** After the refusal the rule is
  absent, so no delete is planned. A restart that reloads the file lists it
  again, which clears the marker; then exactly one delete goes out. If that
  is refused again, the rule is absent again, and nothing more is sent
  until the next restart.
- **Refused edited-copy removal** (`remove`): the copy is gone from memory,
  as after success. So the choices record `user_removed` there too.
  Otherwise the next pass would install the canonical rule over a copy the
  user asked to remove.
- **Refused `apply` delete:** recorded as removed (`choices.removed`), with
  status `OffFileLeft` and no sticky failure, so no "Not removed." flicker
  appears before the follow-up pass.

### 3. Rules page and editor (`bridge-cli/rule_commands`)

- A `DeleteRule` the daemon refuses gives `OkWithNote`, in the style of
  rename's `OLD_FILE_LEFT`: "Deleted. The firewall stopped using this rule,
  but couldn't remove its saved file, so the rule may come back when the
  firewall restarts. If it does, delete it again on the Rules page.
  (reason)".
- `republish` runs for anything but `Ok`, so the list goes out without the
  row.
- Kirigami's Rules page sends deletes without a `request_id`, so it never
  sees that result. It sees the `leftOnDisk` sentence instead, persistent
  while the marker stands. The editor shows `OkWithNote` text as is
  (PlainText).
- `rename.rs` drops its own `cache.remove(old)`: the central handling does
  it before the waiter is told.

### 4. Blocklists and profiles

- **Blocklist sink:** a refused delete also drops the name from
  `confirmed`. `in_place` consults `confirmed` while the cache is
  `Unknown`, so it could otherwise claim a rule the daemon no longer holds.
- #107's "unsubscribe with a refused delete removes the list files at once"
  is unchanged.
- `remove_other_kinds`' text "Snitchwatch will try again" now means: when
  the rule comes back at a restart (it's no longer listed). The text is
  reworded to say that, and so is the unsubscribe's refusal text. Both now
  say the rule stopped applying but its saved file may bring it back.
- **Leftover blocklist rules** (#73, the Blocklists page's "Remove"): a
  refused delete takes the rule off the leftover list too.
  - `removal_note` counts it as removed and adds a sentence about its file.
  - The note shows only while leftovers remain. A pass where every delete
    was refused leaves none, so the Rules page's `leftOnDisk` sentence is
    what tells the user.
  - The tests that used refusals to produce a lingering note now use a
    silent daemon. A new `RefuseThenSilent` harness answer covers the mixed
    note.
- **Profile enforcer:** a refused purge is logged, as before. The rule
  leaves the cache, so the next pass doesn't resend. A restart that reloads
  the file brings it back and it is purged then.

### 5. Mock daemon (`tests/mock_opensnitchd/src/loader.rs`)

A `LoaderModel` (memory, files, stuck files), modelled on bridge-cli's
`test_daemon::LoaderModel`, in stock order:
- `DELETE_RULE` drops from memory first, then fails on a stuck file.
- A name not in memory gets `OK` and nothing else.
- `CHANGE_RULE`:
  1. always → temporary removes the old file;
  2. an enabled rule that fails `validate_rule_shape` gets `ERROR`, with the
     old rule kept;
  3. otherwise memory takes the new rule, and an `always` rule's file is
     written, unless the file is stuck (`ERROR`, memory already changed).
- `restart()` reloads memory from the files in the daemon's reported shape
  (`round_trip::as_daemon_reports`), and serves that as the next
  `Subscribe` snapshot.
- `spawn_loader_responder` answers a stream from it. Dropping its receiver
  stands for the daemon process exiting: the next command is neither
  applied nor answered, and the stream closes. On TCP the bridge fans a
  command out to every open stream, so a lingering old stream would
  otherwise apply it to the shared model a second time.

## Tests (TDD: each written RED first)

- **Cache unit tests**
  - a refused delete removes the rule and sets the marker;
  - a snapshot listing the name clears it, and one lacking the name keeps
    it;
  - an `always` `CHANGE_RULE` `OK` clears it, and neither a temporary one
    nor a `DELETE` `OK` does;
  - disabled + `always` refused upserts;
  - enabled refused leaves the cache unchanged;
  - the cap holds;
  - the revision bumps;
  - `RulesNotShown` carries the count.
- **`daemon_commands`:** an `ERROR` reply removes the rule before the waiter
  is told; a late `ERROR` within grace does too.
- **Curated end to end** (`bridge-cli/tests/curated_defaults.rs`, mock
  loader model): the tower sequence.
  1. Install, then a refused delete: status `OffFileLeft`.
  2. Turn on: a `CHANGE_RULE` **is** sent, and the model holds the rule
     again.
  3. Turn off: a `DELETE_RULE`; the model's file is gone.
  4. Nothing more is sent.

  A second test covers the restart: after a refused delete, restart the
  model (the file reloads). One delete goes out and removes the file.
- **Rules page:** a refused `DeleteRule` leaves the row out of `SetRules`,
  the result is `OkWithNote` with the honest note, and `leftOnDisk` is 1.
- **Blocklists:** a refused delete drops the name from `confirmed`. The
  existing #107 tests pass.
- **Profiles:** a refused purge isn't resent on the next pass.
- **Kirigami:** `not_shown_text` with `left_on_disk`; `status_text` for
  `OffFileLeft`.

## Open question (for the owner)

**`CHANGE_RULE`, enabled + `always`, answered `ERROR`.** On stock v1.8.0 it
is one of two cases:
- **A compile error.** Nothing changed: the old rule is still in memory and
  on disk.
- **A `Save` failure** (an immutable, read-only or full rules directory).
  The new rule **is in memory and applies** until the next start. The file
  holds the old version, no file, or a truncated file that won't load.

Only the daemon's error text tells these apart ("(2) error compiling …" vs
"Error while saving rule …"), and this change doesn't trust that text. The
cache keeps its current copy. That is what the daemon holds after its next
start in both cases (unless a write truncated the file), and it is today's
behaviour.

The residual risk: in the `Save` case an `allow` can apply while the GUI
shows the old state. One example is turning a recommended rule back on
while its file is still immutable. It shows "Not installed", though the
rule applies until the daemon restarts.

The fork's pre-`Replace` refusal of `lists` rules has the same shape and
changes nothing, so no marker is set for this case. A marker would put a
permanent spurious note on every refused blocklist.

A follow-up could show a per-row "unconfirmed" state. That needs a wire
field and a Kirigami row role; it is out of scope here.

## Known residue (not changed here)

- An editor change of a disabled `always` rule that the daemon refuses
  (`Save` failed) now shows in the list as the daemon holds it, the new
  version. The editor's result still reads "Not saved: <daemon text>",
  which is literally true: it applies until the daemon's next start.
- `rules.rs` was already over the 800-line guide (834 lines) and grows by
  about 30 lines. The new logic lives in `rules_refused.rs`.

## Not changed

- No text parsing of daemon errors.
- No re-sending of a refused delete.
- Prompt verdict upserts don't touch the marker.
- The marker isn't saved across bridge restarts.
