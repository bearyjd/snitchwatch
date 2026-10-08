# Default-applied connection events (E3)

**Date:** 2026-10-08
**Option:** E3 of `2026-10-08-prompt-slot-ux.md` ("Record default-applied
connections as events"), requested from bazzite-tower under S4.
**Baseline:** `main` @ `c35cc66`.
**Size:** S–M. Bridge (translator, rule-hit counts), Kirigami (connection
list and inspector), `mock_opensnitchd`, one integration test.

## Problem

opensnitchd applies its `DefaultAction` to a connection no rule matched:
when an Ask fails or times out, or while the prompt slot is busy. Stock
v1.8.0 appends no `Statistics.events[]` entry for such a miss
(`stats.go` `onConnection` returns before the append when `wasMissed`), so
the bridge can only count misses (`rule_misses`, prompt-slot Part A). It
can't list them.

## The contract (agreed with the bazzite-tower fork, 2026-10-08)

The daemon side is **bazzite-tower PR #89** (branch
`feat/snitchwatch-daemon-prompt-slot`, stacked on its #88; approved, not
merged). The fork reports each connection that got the daemon's
`DefaultAction` as an ordinary `Statistics.events[]` `Event`. Its `rule`
is synthetic:

| field | value |
|---|---|
| `name` | `""` |
| `description` | `"snitchwatch:default-action"` (exact; the marker) |
| `action` | the action actually applied: `allow`, `deny` or `reject` |
| `duration` | `once` |
| `enabled` / `precedence` / `nolog` | `true` / `false` / `false` |
| `operator` | `{type: "simple", operand: "true", data: "", sensitive: false}` |
| `created` | the event time |

- Each such event is exactly one `rule_misses` increment, **never** a
  `rule_hits` one.
- **Keyed on both halves.** A marked event has `name == ""` **and**
  `description == "snitchwatch:default-action"`, matched exactly
  (case-sensitive, no trimming).
  - Stock v1.8.0 never emits this. It can, however, load a hand-written
    rule file named `""`, and such a rule's events are real rule hits.
  - The daemon version string is `1.8.0` on both, so it can't tell them
    apart.
- **Unmarked `""`-named events keep today's behaviour:** a decided row
  with `matched_rule: Some("")`, counted in `rule_hits` arithmetic.
- **E2** (`DropWhileAsking`, a daemon config-file key, off by default)
  needs no bridge code. A busy drop touches no statistics: no event, no
  `rule_hits`, no `rule_misses`.
- **Eviction differs.** On the fork, a miss that records no event no
  longer evicts an older event from a full ring; on stock it still does.
  The gap arithmetic below doesn't depend on either, and no test assumes
  one.

## Decisions

### One predicate

`translator::connection::is_default_action_rule(&Rule)` and the const
`DEFAULT_ACTION_MARKER`. `event_to_row`, `RuleHits::record` and the tests
all use it, so the marker is defined once.

### Wire representation

- A marked event becomes a **decided** row:
  - `action`: the normalized action (`reject` folds to `deny`, as for
    every event row);
  - `matched_rule: None`. There is no rule named `""` to point at;
  - a new additive field, `decided_by_default: bool` (JSON
    `decidedByDefault`), set to `true`. It follows `deferred`:
    `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`, so it
    is absent on every other row and in old payloads.
- The row id stays `event-<unixnano>`.
- **Why a flag and not `matched_rule: None` alone.** Old rows and some
  decision paths already have `None`. A GUI must never infer "the default
  decided this" from a missing name.
- **Why not reuse `deferred` / `auto_answer`.** Those describe a *bridge*
  prompt that was put off. A marked event can come from a connection the
  bridge never saw (slot busy, no GUI authenticated).

### What older clients show for a marked row

- **Kirigami on `main`** (no `decidedByDefault`): `ConnectionRow` has no
  `deny_unknown_fields`, so the field is ignored.
  - The row reads as decided: "allowed" / "denied".
  - Inspector "Matched rule: default action", the existing fallback in
    `matched_rule_display` for a decided row without a name.
  - No "Show rule" (`matchedRule` is empty) and no "Make a rule…" (not
    deferred).
  - That is correct, only less specific.
- **The web UI** never reads `matchedRule`; it sees the action only.
- **An older bridge with the fork's daemon** (not shipped here, noted for
  completeness):
  - it builds `matched_rule: Some("")`, which Kirigami also shows as
    "default action" with no "Show rule";
  - but it counts marked events as received, so every ping with one marks
    a hit-count gap.

### Rule-hit counts (`cache/rule_hits.rs`)

- `missing = Δrule_hits − received` now uses `received` = the events that
  are **not** marked. A marked event grows `rule_misses`, not `rule_hits`.
  Without the exclusion every default-applied connection would read as a
  lost event.
  - Ruleless and unmarked-`""` events still count in `received`.
- **No check on `rule_misses` growth.** It also counts retransmits and
  unanswered packets (prompt-slot doc), so it can't be balanced against
  marked events.
- **The cap.** With the fork, a marked event takes a slot in the daemon's
  `MaxEvents` buffer.
  - A rule event pushed out by it still shows as `missing > 0`, a gap.
  - A marked event pushed out decides no rule's count, so it is no gap.
- `record` already skips events with an empty rule name. A marked event
  always has one, so it never counts as a hit for any rule. A test pins
  this, with no extra code.

### Other consumers

- **`prompt_slot`** reads only `rule_misses` and `uptime`: unchanged.
- **`ConnectionCache`** inserts event rows with `insert_decided`. A marked
  row has an action, so the debug assertion holds.
- **`grpc_server::ping`**: unchanged apart from its comment ("matched
  against a pre-existing rule" is no longer the only case).
- **Kirigami grouping/filter** use the action: the row counts as allowed
  or denied in group totals.

### A bridge prompt that the daemon then defaulted

- **No stale pending row can sit next to the new row.**
  - The bridge settles its own prompt at `ANSWER_TIMEOUT` (30 s), before
    the daemon's 120 s `AskRule` deadline. `answer_unanswered` turns the
    row **deferred**, not pending, and then the daemon applies its default
    and (fork) reports the marked event.
  - An Ask the daemon drops or cancels removes its row through
    `PendingCleanup`.
  - "Decide later" without a bindable program (P-a) defers the row the
    same way.
- **What remains is two rows for one connection.** The deferred "Not
  answered in time…" row, and the new default-action row.
- **The cache can't correlate them.**
  - `ConnectionRow` carries no source port or pid, and the cache keeps no
    source tuple per prompt.
  - Process + destination + port isn't unique: the waiting connection's
    retries, and other connections of the same program, get the default
    too and produce marked events with the same fields.
  - So nothing is merged. Owner question below.

### Kirigami

- **`outcome_text`** gains a label for `decided_by_default` rows. It is
  the list's verdict label (flat and grouped) and the inspector's Verdict
  line, with no QML change:
  - "Denied (the firewall's default action)";
  - "Usually allowed (the firewall's default action)". "Usually" follows
    the prompt-slot rule for default-action allows (tower's r8: requeued
    packets can drop under nftables chain churn).
- **`matched_rule_display`** checks the flag first:
  - "No rule: the firewall's default action (deny)" / "(allow)";
  - it never blanks, and never infers this from `matched_rule == None`.
- **No rule actions.** "Show rule" needs a non-empty `matchedRule`, and
  "Make a rule…" a deferred row; a marked row has neither. A QML probe
  pins both. No QML change.

## Overlap with PR #101 (`feat/rule-insights-badges`, in flight)

- `cache/rule_hits.rs`: this PR changes `record` / `missed_events` and the
  module doc. #101 changes `adopt_snapshot` (a lost count is a gap).
- `cache/rule_hits/tests.rs`: new tests are appended at the end. #101
  inserts after `a_new_snapshot_without_a_rule_drops_its_count`.
- **Behaviour.** #101's "unused" badge counts from the last gap. Without
  the exclusion here, every default-applied connection on the fork would
  reset that window.
- Kirigami: #101 touches `rules/*`, `RulesPage.qml` and the simulator's
  decider; this PR touches `connections/*` only.

## Tests first

1. **Translator** (`translator/connection.rs`):
   - a marked `allow` / `deny` / `reject` event becomes a decided row with
     the normalized action, `matched_rule: None`, `decided_by_default`;
   - a `""`-named event without the marker keeps `Some("")` and no flag
     (description half);
   - a named rule with the marker description is an ordinary rule row
     (name half).
2. **Wire** (`ws_messages.rs`): `decidedByDefault` is absent when false
   and present when true; a marked row has no `matchedRule` key; a payload
   without the field parses as false.
3. **Rule hits** (`cache/rule_hits/tests.rs`, explicit counters):
   - stock pings: unchanged (existing tests stay green);
   - a baseline, then pings of only marked events and of a mix, with
     `rule_hits` grown by the unmarked ones: no gap;
   - a real gap alongside marked events is still noted;
   - a marked event counts for no rule;
   - a named rule carrying the marker description counts, and counts in
     `received`;
   - an unmarked `""` event that grew `rule_hits` is no gap.
4. **Kirigami unit** (`connections/outcome.rs`, `connections/row_store.rs`):
   the labels above; the flag wins over `matched_rule`.
5. **Kirigami QML probe** (`tests/default_action_rows_qml.rs`): a real
   `ConnectionsPage` + `ConnectionsModel` with default-decided rows:
   - the list label, flat and grouped;
   - the inspector's Verdict and Matched rule;
   - no "Show rule", no "Make a rule…".
6. **`mock_opensnitchd`**: `default_action_event(...)` builds the contract
   `Event`; a unit test pins its shape.
7. **Integration** (`tests/default_applied_events_test.rs`, the
   `bridge_protocol_test.rs` pattern): pings carrying marked events reach a
   WS client as `decidedByDefault` rows, and `requestSnapshot`'s
   `ruleHits` reports no gap.

## Mutation checks

Each flipped alone, a test must fail, then reverted:
- the name half of the predicate;
- the description half;
- the gap exclusion (back to `events.len()`);
- `outcome_text` and `matched_rule_display` ignoring the flag.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D
warnings`; `cargo test -p snitchwatch-bridge`; `mock_opensnitchd` tests;
Kirigami tests headless (`QT_QPA_PLATFORM=offscreen
QT_QUICK_CONTROLS_STYLE=Basic`); `cargo test` for default members.

## Open question for the owner

- **Fold the marked event into the bridge's own deferred row?** The bridge
  could keep the Ask's source tuple and pid server-side for deferred rows,
  and when an exactly matching marked event arrives, fill in the deferred
  row's action (useful when the daemon config was unreadable) instead of
  adding a second row. Not built here.
- **"Make a rule…" on a default-decided row?** Today it is offered only for
  deferred rows. A default-decided connection is a natural candidate, but
  that widens #98's gate, so it is left out.
