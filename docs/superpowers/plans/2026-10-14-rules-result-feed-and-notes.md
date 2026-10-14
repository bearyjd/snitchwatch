# Rules page result feed and refusal notes: #152, #153, #154, #120 item 7, #151

**Date:** 2026-10-14
**Source:** issues #152, #153, #154, #151; #120 item 7 (owner decision
2026-10-14: no per-row state, a one-time result note).
**Baseline:** `main` @ `4ead981`, branch `fix/round2-rules-notes`.
**Size:** M. Bridge-cli `rule_commands*`, bridge `cache/rules_refused.rs`
(one helper), Kirigami `RulesModel` + a Qt-free `rules::command_notes`,
`RulesPage.qml`, one flaky probe.

Stock semantics (`vendor/opensnitch/daemon/rule/loader.go`, v1.8.0):
`Replace` = `replaceUserRule` (compile only if enabled, then store in memory)
then `Save` for `always`. So a refused `CHANGE_RULE` of a rule that is:
- disabled + `always`: only `Save` failed; applied in memory, not saved
  (`RefusedEffect::AppliedNotSaved`, already decided by the cache);
- enabled + `always`: compile error (not applied) or `Save` failure
  (applied); the reply can't tell (`RefusedEffect::Unknown`);
- temporary: only compile can fail (not applied).

## Items

- **#153** A refused Add/Edit of a disabled `always` rule answers
  `OkWithNote` with a fixed note (applied, not saved; the saved version, or
  none, loads at the next restart). Decided by `applied_but_not_saved` on
  the sent notification, as #150 item 3 does for a turn-off. Paths:
  `Task::Send` (add), `edit::run` (the non-"lost" refusal).
- **#120 item 7** A refused `CHANGE_RULE` of an enabled `always` rule
  (add, edit, Rules-page turn-on) answers `Unsure { reason }` with a fixed
  "may or may not have applied" note plus the daemon's reason. Not
  `Rejected` (every GUI reads that as "not changed": the editor says
  "Not saved", Make-a-rule says "not created") and not `OkWithNote` (read
  as saved/created). Every GUI already treats `Unsure` as unknown (editor:
  not saved, shows the reason, sheet stays open; Make-a-rule:
  `Ending::Unknown`). The cache stays as it was (`note_maybe_applied`), and
  the list is re-sent (as for any non-OK). No wire field, no row state.
  Recommended rules keep their own path (item-12 undo; `maybe_applied`).
- **#154** `toggle_curated`: a refused turn-off answers `OkWithNote` with
  `TURNED_OFF_FILE_KEPT` (curated rules are always `always`; the cache
  already shows it off via `apply_refused`). The item-12 undo of a refused
  turn-on is untouched.
- **#152** `RulesModel.setEnabled`/`deleteRule` send a request id
  (`rules-…`) and return it. A result-only feed (`spawn_result_feed`, live
  session only) feeds a Qt-free `rules::command_notes::CommandNotes`
  (bounded pending ids; all dropped when the model sees a new bridge
  session, since a result goes only to the asking connection). Each
  awaited result sets the `commandNote` property (plain text; Ok clears
  it; OkWithNote shows the note; failures say why, reusing
  `editor_view::finished_with` with a Rules-page wording). `RulesPage.qml`
  shows it as a PlainText label in the header and refreshes the inspector
  when it changes (a refusal with a request id no longer re-sends the list,
  so the inspector's moved switch must be re-read). Sending clears the
  note. QML probe: toggle -> OkWithNote note shown; stale id ignored.
- **#151** The host-row probe advances a phase only once every delegate
  of the expected count exists (observed state), under a 10 s watchdog,
  instead of a fixed 100 ms phase timer. Stability: 30 repeated runs, and
  under CPU load.

Out of scope (report): rename's first-step refusal of a disabled/enabled
`always` new rule (both may exist) keeps answering `Rejected`.

## Tests first

- bridge-cli: add/edit of a disabled `always` rule refused -> OkWithNote;
  add/edit/turn-on of an enabled `always` rule refused -> Unsure; a
  temporary rule refused -> Rejected; curated turn-off refused ->
  OkWithNote, turn-on still Rejected and undone.
- kirigami: `CommandNotes` unit tests; a RulesPage QML probe.

## As built

- #153, #154, item 7: `aaad4a2` (`rule_commands/refused.rs`, `IfRefused`;
  `cache::rules::may_have_applied`). Three older tests that pinned
  `Rejected` for an enabled `always` refusal now use a temporary rule, the
  one shape still definitely not applied. Mutants: 5 of 6 killed; the
  survivor (count disabled `always` as maybe-applied) is equivalent, since
  that shape is decided as applied-not-saved first.
- #152: the note carries its rule (`commandNoteRule`), so the inspector
  shows it only on that rule. No session drop of waiting ids (a click right
  after a reconnect would lose its result); old-session results never reach
  the model (`result_feed`), and the wait list is bounded (32). A send that
  never left the GUI leaves no note, as before. No new roles. Probe mutants
  (note on any rule, no refresh on result, any id counts, note kept on
  send): all killed.
- #151: pin-only. The old probe passed 30/30 idle, 30/30 under 44 CPU
  burners, and 30/30 with a 1 ms tick; the gated one passes the same.
- Not done: rename's first-step refusal of an `always` new rule, and
  rules import (`ImportOutcome::Rejected`), still answer "rejected" where
  the daemon may have applied the rule.
