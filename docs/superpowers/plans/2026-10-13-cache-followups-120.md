# Rules cache follow-ups from #120 (items 2, 3, 4, 6, 10, 11, 29, 30, 33)

**Date:** 2026-10-13
**Baseline:** `main` @ `bbc5778`, branch `fix/followups-120-cache`.
**Size:** M. Bridge (rules cache, rule commands, blocklists leftovers),
Kirigami (one blocklists property, one page label). One additive wire field.
**Out of scope:** items 7 and 22 (owner decisions), `curated/*` (items 9-20,
executor A), rule hits / N3 / test hygiene (executor C).

Each item was checked against `main` first. #127 already fixed item 1
(cache revisions: only a snapshot *received after* a refusal clears a
marker); nothing here undoes that.

## 6. `cache/rules.rs` over 800 lines (done first, pure move)

`rules.rs` was 963 lines. `PendingSnapshots`, `RulesSync`, `PublishHold`,
`publish_rules`, `settle_rule_command` and `prune_expired_rules_every` move
to a child module `rules_sync.rs`, re-exported at their old paths. The only
other changes are visibility (`pub(super)` on the fields the sibling
`count`/`refused` modules and the rules tests read) and imports. Result:
514 + 469 lines.

## 4 + 10. A "file may remain" marker that outlives its file

What already clears a marker (`RulesCache::files_left`): a snapshot received
after the refusal that lists the name (#127), and a confirmed `always`
`CHANGE_RULE` (it rewrote the file, `apply_confirmed_at`). The "confirmed
`always` write" half of the item is therefore **already done**.

A confirmed `DELETE_RULE` must **not** clear a marker on its own. Stock
`Loader.Delete` (`rule/loader.go`) answers `OK` without touching the file
when the name isn't in memory, which is exactly the state a refused delete
leaves; curated's retry sends that harmless no-op delete, and clearing on it
would hide a real leftover (the class of bug #127 fixed). Two tests pin this
already (`only_a_confirmed_always_change_clears_the_marker`,
`confirmed_file_operations_after_readoption_keep_their_marker_semantics`).

The one provable case: the daemon re-stored the name as an `always` rule
**after** the mark. A refused disabled-`always` `CHANGE_RULE`
(`RefusedEffect::AppliedNotSaved`) is such evidence: `replaceUserRule` put
the rule in memory, only `Save` failed. A later delete `OK` of that name then
went through `deleteRuleFromDisk` (`os.Remove` of `<name>.json`) and
succeeded, so the file is gone: clear the marker. The evidence ends with a
confirmed non-`always` change (memory then holds a temporary rule; `Delete`
removes no file) and with a new refused delete (re-marked).

What cannot be known from the replies, and stays as an over-warning (the
safe direction):
- item 10's shape: a refused **enabled** `always` install is
  `RefusedEffect::Unknown` — a compile error (old rule kept, not in memory)
  and a `Save` failure (new rule in memory) answer the same `ERROR`, so a
  later delete `OK` proves nothing in the cache. Curated knows its own rule
  compiles; any curated-side clear belongs with items 18-20 (executor A).
- an `ENOENT` refusal (the bridge never reads the daemon's text);
- a file removed by hand, or re-loaded live by the daemon's watcher;
- a prompt verdict that rewrites the file (no reply comes back for it).
These are written into the `files_left` doc comment.

## 11. A rename whose old-name delete is refused publishes the list 3 times

Now: the new name's `OK` publishes, the refused delete's `apply_refused`
publishes, and `Answer::finish` republishes because the outcome isn't `Ok`.
Fix:
- `rename::run` holds publishes (`DaemonCommands::hold_publishes`, as the
  import and the curated pass do) across both steps and drops the hold
  before replying: one `SetRules` with both steps' effect. Cost: while a
  rename waits for the daemon (at most two reply timeouts), other commands'
  `SetRules` wait too; they still apply to the cache at once.
- `Answer::republish` skips `OkWithNote` as well as `Ok`: every
  `OkWithNote` (refused delete, rename's old file left, item 3) means the
  change stands and the list already shows it.
Test first: count the `SetRules` before the result in the refused-old-delete
rename test (3 now, 1 after).

## 3. A refused toggle-off of an `always` rule says nothing

A disabled rule isn't compiled, so for an `always` one only `Save` can fail,
after the rule as sent replaced the old one in memory
(`RefusedEffect::AppliedNotSaved`; the cache already shows it off). Its file
still holds the enabled version, which loads at the next restart. The
result today is `Rejected`, which is wrong (the rule *is* off).

Fix: a pure toggle-off (`Plan::Send` of a `CHANGE_RULE`) answered `ERROR`
whose shape the cache reads as `AppliedNotSaved` gets `OkWithNote` with a
fixed text ("Turned off. … may come back on when the firewall restarts …"),
plus the daemon's reason in brackets as the refused delete does. The shape
predicate is shared with the cache (one public fn), so the note and the
cache can't disagree. Not via the `files_left` marker: the rule is still
listed, the next snapshot would clear the marker while the file still
holds the enabled version, `leftOnDisk` wording is about deleted rules, and
curated's reserved-name guards key on `files_left`. No new Kirigami role:
the Rules page already shows `OkWithNote` notes. Recommended-rule toggles
(`toggle_curated`) are executor A's.

## 2. Blocklists page silent when every leftover delete is refused

Each refused delete unlists its rule (the daemon dropped it from memory), so
`leftover_count()` becomes `None` and three things hide `removal_note`'s
text: `leftover_outcome`'s `leftover_count()?` gate, the nothing-left branch
of `announce_leftover_change` (clears it), and the page's
`visible: leftoverRules > 0`.

Fix: an additive field on `SetBlocklistLeftovers`, `filesLeft` (plain text,
omitted when `None`, `skip_serializing_if`), set when a removal ends with
nothing left and at least one refused delete. Fixed text, no daemon text:
"The leftover rules were removed, but the firewall service couldn't remove
the saved files of N, so they may come back when it restarts." Lifetime
(pinned in a test): until the next removal, or until leftovers are counted
again (the files came back), or the bridge restarts — a GUI that connects
meanwhile still gets it. Kirigami: `leftoverFilesLeft` property on the
blocklists model, a label with `objectName: "leftoverFilesLeftText"` shown
when no leftovers are counted and the text is non-empty, and a QML probe
test next to `tests/blocklists_leftovers_qml.rs`.

## 29. The `renamed` allowance never shrinks

Ending it at a timed answer's expiry is **wrong**. Trace (`loader.go`
`addUserRule` → `setUniqueName`): the bridge counts `renamed` only when its
list already holds `<name>`, and `RulesSync::upsert` *replaces* that row
with the answer. The daemon holds the original `<name>` plus `<name>-2`. At
the answer's expiry the daemon drops `<name>-2` and the bridge prunes its
`<name>` together; the daemon still holds the original `<name>` (e.g. the
disabled rule that made the program ask again), so the list is still one
short. Ending `renamed` there would raise the hint after every
disable → re-prompt → timed answer. A test pins this first.

Instead, shrink on the daemon's count: after a debounced run
([`PINGS_TO_CLEAR`] equal agreeing readings), set `renamed` to at most
`reported - expected`. The other allowance reasons (refused adds, pruned
slack) are room for *more* rules, so they can't hide a missing one; the
only way the count reads low, a listed or left-out temporary rule's timer,
already makes such readings no evidence (`timers_may_drop_rules`). (As
implemented: an earlier draft also required no refused adds and no pruned
slack; mutation testing showed those guards change nothing, and the
reasoning above says why.) Every #124 guard keeps its test green
(disable → re-prompt → Always `-2`, refused add applied anyway, suspend).
If the guarded version turns out not to fire in a useful case, it is
reported as an owner decision instead.

## 30. Stale #65 plan wording

`2026-10-09-rules-count-hint-65.md` says the pruned-early allowance "in the
normal case notes nothing". Since #124, a snapshot rule with an old
`created` keeps a tolerance of up to min(age, duration) + 5 s after the
prune. Reword; check the `MayHold` / `Expiry` docs for the same claim.

## 33. `PendingReply::drop` doesn't move the waiter to `late`

Reachable only if a production future holding a `PendingReply` can be
cancelled before `wait` finishes. Holders: `settle_rule_command` (spawned),
rule commands / rename / edit (spawned tasks), the curated pass, the
profiles enforcer, the blocklist daemon sink and leftover removal (each
awaited to the end in its own task or worker loop; the blocklist worker's
`select!` arms aren't cancelled). Shutdown drops don't matter (the cache
goes too). If no cancellable holder is found: a pin test of today's
behaviour (a dropped reply's late `OK` is ignored) plus a doc comment on
`Drop` naming the holders; no fix.

## Order and gates

Plan → 30 → 11 → 3 → 4 → 33 → 2 → 29. Each behaviour change test-first
(seen failing). Gates: `cargo fmt --all --check`, `cargo clippy
--all-targets -- -D warnings`, `cargo clippy -p snitchwatch-kirigami
--all-targets -- -D warnings`, `cargo test -j 4 --no-fail-fast`, the
Kirigami headless suite, `just package-check`.
