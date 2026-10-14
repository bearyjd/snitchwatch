# Curated defaults: refused-install rollback (#130) and #120 round 2

**Date:** 2026-10-13
**Source:** issue #130 (bridge half; the daemon half is bazzite-tower PR
#109), issue #120 items 10, 12, 18, 19, 20, 38, 42.
**Baseline:** `main` @ `bbc5778`, branch `fix/followups-120-curated2`.
**Size:** M. Bridge: `curated/manager.rs`, `curated/manager_state.rs`,
`curated/manager_intent.rs`, `curated/reconcile.rs`, `curated/store.rs`,
`curated/mod.rs`, `accounts.rs`; bridge-cli: `rule_commands.rs` (item 12),
`lib.rs` (lookup wiring). Tests: new bridge unit test files, bridge-cli
end-to-end on the mock loader model. No wire or Kirigami change.

Kept as they are: the request counter in `PassKey` (#132), the generation
stop, no retry until a change, publish holds, the busy re-queue, and the
#128 consent tickets (broad DNS needs a ticket).

## #130 (first): a refused install is rolled back and its consent dropped

Stock v1.8.0 `replaceUserRule` takes the rule into memory before `Save`
fails, so a refused install may apply although the GUI shows "Not
installed", and the choice (and, for DNS, the grant) stays on, so a later
pass or reconnect can install it again without fresh consent.

- `apply`, new arm before the generic `Err`: a `Done::Install` the daemon
  refused (`daemon_refused`), whose entry is still on:
  - `revoke_locked` (bumps `broad_revision`, drops the grant), then
    `keep(choices.disable(id))`: the entry is off, saved, and a later
    reconnect that lists the rule deletes it (a disabled unedited copy);
  - no `fail()`: a sticky failure under the same id and generation would
    make `without_failed` drop the delete planned next;
  - status `NotInstalled` for this pass, and a per-id note
    (`install_refusals`) that survives passes until the entry's next choice,
    so the switch doesn't flip off without a reason.
- The delete is not sent inline: `track_maybe_applied` already recorded the
  name, so the next pass (the choice change bumps `version`) plans one
  `Delete` (`!enabled && maybe_applied`). Its answer drops the record, so it
  is sent once; a refused one goes through the existing #119/#122 arms
  (`OffFileLeft` when a file may remain, `Off` and the cache marker dropped
  when none can).
- Only a refusal. An unanswered install keeps today's behaviour (#120 item
  13's record, deleted when turned off).

## Item 12: a Rules-page turn-on refused on a stuck file

Reachable: installed, its file made immutable, turned off on the Rules page
(`AppliedNotSaved`: memory and cache disabled), turned on there: memory
takes the enabled rule, `Save` fails, `ERROR`; the refusal effect is
`Unknown`, so the cache keeps it disabled while the daemon applies it.

- bridge-cli `toggle_curated`: a refused turn-on is answered, then one
  turn-off of the same rule is sent and its answer only logged. Its own
  refusal is `AppliedNotSaved`, so memory and cache agree (disabled). No
  retry.

## Items 19, 20 and 10: the `OK` delete arm

- 19: a confirmed delete honours the cache marker: `OffFileLeft` when the
  name is marked (or its persisted bit is set), unless the dropped record
  says no file is possible.
- 20: a record with `!file_possible` forgets the marker: in the `OK` arm,
  and in reconcile for a marker a late `ERROR` to a timed-out delete set
  while the record still stands.
- 10: not fixable from the reply. On stock an `OK` delete after a refused
  install removed a real file; on a rollback daemon (tower #109) the name
  wasn't in memory and the file is untouched. The bridge can't tell them
  apart, so it keeps the warning (over-warns only). #130's immediate delete
  shrinks the window. Documented in `reconcile.rs`.

## Item 18: a persisted "file may remain" bit

- `Choices.files_left: BTreeSet<String>` (entry ids), `serde(default)` and
  `skip_serializing_if = "BTreeSet::is_empty"`: older files read as before;
  a file without a bit is still read by older bridges. Counted in `load`'s
  cap and cleaned like the other id sets.
- Downgrade (PR #149 review): the file stays version 3, and `Choices` keeps
  `deny_unknown_fields`. A file saved while any bit is set has `filesLeft`,
  which an older bridge rejects as unreadable: it goes inert
  (`UNREADABLE_REASON`), changing nothing in the firewall, until the file
  is repaired or removed. That fails safe, and the version is deliberately
  not bumped: a bump would make an older bridge reject every newer file,
  not only those with a bit set.
- Set where a refused delete reads `OffFileLeft` (`apply`, `remove`) and by
  item 19's `OK` arm when marked. Cleared on an install `OK` (file
  rewritten), an `OK` delete of a listed copy (the file loaded, so it was
  removed), and the no-file case.
- Reconcile merges the bits into `DaemonRules::files_left`, and
  `track_maybe_applied`'s `marked` reads them too, so a file stuck in an
  earlier bridge run still reads `OffFileLeft` and still counts as possible.
- Caveat (module doc): a bit for a file that is really gone (removed by
  hand) stays until the entry is installed again or its rule is listed and
  deleted.

## Item 38: the DNS entry only where `systemd-resolve` is uid 193

- `accounts::uid_of(name)` (`getpwnam_r`, libc is already a dependency);
  `CuratedDefaults` takes an injectable resolver-account lookup, the
  production one by default, and runs it once per pass on a blocking
  thread.
- Not confirmed (lookup error, no such account, another uid): the DNS entry
  is withheld for the pass. Nothing is installed, re-enabled or deleted for
  it; its choices and grant stay as they are (a lookup that fails for a
  moment never deletes a rule the user opted into); status `Unavailable`
  with one fixed plain-text problem. Its intents are refused as
  `Unavailable`. A user's own off still goes through.
- Tests never read the host's account database: the unit harness and the
  bridge-cli curated support inject "193".

## Item 42

`check_protocol` also requires `!sensitive`.

## Tests (each seen failing first, unless marked pin-only)

- #130: e2e on `LoaderModel` (stuck before the first install): one
  `DeleteRule` follows the `ChangeRule`, memory empty, entry off with the
  note, nothing after a restart and reconnect. Marked variant: `OffFileLeft`.
  Unit: DNS refusal drops the grant and needs a new ticket.
- 12: e2e Rules-page sequence above; memory disabled at the end.
- 19/20/18: unit tests on the scripted harness; store round trip and
  backward compatibility.
- 38: unit tests with an injected lookup: withheld (error, other uid), no
  delete of an opted-in copy, back when confirmed.
- 42: unit test on `check_curated_rule`.
- Existing tests encoding the old "refused install stays on" contract are
  rewritten to the new one and listed in the report.
