# Clarity phase 0: exception-first default (0c)

Status: build plan, approved scope. Parent: `2026-10-22-gui-phase0-clarity.md`
section 0c (the spec) and its safety invariant. GUI-only
(`crates/snitchwatch-kirigami`): no wire, bridge, image, unit or daemon config
change. If the work needs one, stop and re-plan.

Owner decision 2026-10-26 (parent open question 3): COLLAPSE the "Background
and system" group, never hide it. A handled row may collapse into that group
with a one-line summary; the group stays visible and expandable.

## Shared rules
- The phase-0 safety invariant is a hard rule: pending, deferred, default-
  decided (`decided_by_default`, or `matched_rule = None` with an action),
  denied, blocklist-decided and unknown-action rows are never collapsed.
  Decisions are per row; a friendly name never decides anything.
- TDD; each clause of the "handled" predicate gets a mutation check (flip it,
  a named test must fail).
- Budgets: `connections/filter.rs` (927 lines with tests) and
  `connections_model.rs` (767) are split first, behaviour unchanged. New QML
  stays under 400 lines and is registered in `build.rs`, `ALL_QML`,
  `CONNECTIONS_PAGE`, `qml_source_guards.rs` and the Connections budget list.

## Step 1: split before adding
Move `filter.rs`'s tests to `connections/filter/tests.rs` and the grouped
toggles out of `connections_model.rs` into `connections_model/toggles.rs`.

## Step 2: the "handled" predicate (`connections/attention.rs`)
`AttentionFacts` holds the names of the rules that are in the CURRENT rules
list, enabled, and start with `CURATED_DEFAULT_RULE_NAME_PREFIX`
(`snitchwatch_bridge::rule_name`), plus the executable paths that have been
seen before (persisted set minus this session's unacknowledged new programs).
A row is handled only if all hold: not deferred; not `decided_by_default`;
`Verdict::of` is Allowed; `matched_rule_name` is one of those rule names; its
`process_path` is in the seen paths. A user rule, a once-off answer
(`snitchwatch-allow-…`), a stale or disabled rule name and a row with no
`process_path` are therefore never handled. With no such rule (rules list empty
or unknown) nothing is handled: the filter is inactive (fail open), so the
unfiltered fast path and `filterActive` stay as today.

## Step 3: the filter
`ConnectionFilter` gains `needs_attention` (in `PartialEq`, ON in the model's
filter by default) and an injected `Arc<AttentionFacts>` (not in `PartialEq`,
like the geo resolver). `matches` drops a handled row only while
`needs_attention` is on. Facts change through a dedicated setter, followed by
`recompute_visible` and a grouped-projection rebuild, because `set_filter`
returns early on equal filters. The model forces this on
`SetRules`/`UpdateRules`, on the baseline and on an acknowledgement.

## Step 4: first-seen state (`connections/seen_programs.rs`)
- In memory, "new this session until acknowledged": after the baseline, a row
  whose `process_path` is not in the seen set flags that path. Only the user
  clears it ("Mark as seen"), and that is also the only way, besides the
  baseline, that a path enters the persisted set: an unacknowledged program is
  never persisted, so it stays new after a restart instead of collapsing.
- Persisted: `paths::config_dir()/seen-programs.json`, a capped (5,000) list of
  `{path, last_seen_ms}`, least recently seen evicted, merged with the file on
  every write (two GUIs can run), written with `private_files::write_private`
  (0600 file in a 0700 dir, atomic replace).
- Load: a valid file owned by this user is the seen set. A missing, corrupt or
  foreign-owned file re-baselines with the visible notice every time; the
  owner check takes the expected uid as a parameter so it is testable without
  `chown`.
- Baseline signal: the first time in the model's life that a `SetRules` is
  applied after a `ClearConnectionRows` has been applied to the row store. The
  bridge answers every connect's `RequestSnapshot` with `ClearConnectionRows`,
  the row replay (`InsertConnectionRows`, omitted when empty), then `SetRules`
  (`snitchwatch-bridge-cli/src/lib.rs`). In grouped mode rows are debounced
  while rules apply at once, so a rules message flushes the buffered rows
  first; then arrival order is application order. Only when the load
  re-baselines are the rows present at that moment recorded as seen; with a
  valid file they are checked like any later row.
- Notice (one time, dismissable): "Baseline recorded: programs seen so far will
  not be flagged as new; a program already running at that moment was
  baselined." preceded by why the list was not read when it existed.
- A reconnect (`ClearConnectionRows` then replay) changes neither the flag nor
  the set: there is one baseline per model life.

## Step 5: UI (`BackgroundGroupBar.qml`, hosted by `ConnectionsFilterBar.qml`)
- A fourth switch, "Full list", the inverse of `filterNeedsAttention`.
- While collapsing: "Showing N of M · Show all" (matching vs total rows).
- The "Background and system" group row, always shown while recommended rules
  are known: "N connections from K programs, allowed by recommended rules"
  with Expand/Collapse (one model bool underneath: the same as the switch).
- "N new programs this session are always shown · Mark as seen", and the
  baseline notice with Dismiss.

## Tests
- `attention` unit tests: each excluded row kind, stale/disabled rule,
  once-off answer, host-only user rule, first-seen, `process_path` None, empty
  rules; a mutation run over every clause.
- `seen_programs` unit tests: round trip, cap and LRU eviction, merge on
  write, corrupt and foreign-owned files re-baselining with the notice,
  missing file, reconnect not resetting, baseline before/after snapshot.
- `filter` unit tests for `needs_attention`; a QML test that the switch,
  "showing N of M", the group row and the notice drive the model.
- Screenshots via `just kirigami-screenshots` before and after; demo mode seeds
  a recommended rule and an in-memory seen set (never writes the file).

## Review fixes (2026-10-27, /codex on #234)
- A `ClearConnectionRows` revokes collapsing (empty facts, counts cleared)
  until the new session's `SetRules`; an `UpdateRules` does not restore it.
  The seen set and the new-program flags are kept.
- Writes hold an advisory `flock` on `seen-programs.json.lock` from the read
  to the replace (`learned::acquire_lock`, shared with `learned/store.rs`);
  a contended or unsupported lock degrades to last-writer-wins with a log.
- A program seen before that is seen again has its last-seen time saved at
  most once a minute and when the model is dropped; unacknowledged programs
  are never saved.
- While collapsing is the only narrowing, the flat view re-decides each
  changed row (`row_store::incremental`, ranged signals, no reset, no
  re-filter), the grouped path updates `visible` the same way, and the
  group's figures are kept per row (`attention::HandledCounts`). With the
  full list showing nothing is counted, and the expanded group says its rows
  are in the list "subject to the other filters".
- A seen file that others may write (mode not 0600), or one in a directory
  that is not ours and 0700, re-baselines with the notice.
