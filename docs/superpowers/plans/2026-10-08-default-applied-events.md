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
- **The marker can be spoofed** (PR #108 security review, L1):
  - by root, with a stock rule file named `""` that carries the
    description;
  - by a local process on the legacy TCP transport (#35).
  - It grants nothing those actors couldn't already do: such an event only
    shows a decided row and stays out of the hit-count arithmetic. Tower
    has been asked to confirm that its rule loader refuses empty names.
- **Off-contract actions.** A marked event whose action isn't `allow`,
  `deny` or `reject` logs a warning the first time in a bridge run, and its
  row folds the action like any event's (`deny` unless `allow`); a test
  pins that.

## Decisions

### One predicate

`daemon_contract::is_default_action_rule(&Rule)` and the const
`DEFAULT_ACTION_MARKER`, in a small module of their own. `event_to_row`,
`RuleHits::record` and the tests all use it, so the marker is defined
once.

### Wire representation

- A marked event becomes a **decided** row:
  - `action`: the normalized action (`reject` folds to `deny`, as for
    every event row);
  - `matched_rule: None`. There is no rule named `""` to point at;
  - a new additive field, `decided_by_default: bool` (JSON
    `decidedByDefault`), set to `true`. It follows `deferred`:
    `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`, so it
    is absent on every other row and in old payloads.
- The row id is `event-<unixnano>-<seq>`, `<seq>` from a bridge counter
  (PR #108 security review, L2). The daemon's time alone can repeat, and
  "Make a rule…" finds its row by id when clicked. Nothing parses the id.
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
  - So nothing is merged.
- **Decision (orchestrator, 2026-10-08; for the owner to review): keep
  both rows in v1.** This is a known limitation. Folding the marked event
  into the deferred row would need the Ask's source port and pid kept on
  the bridge side, so an exactly matching event can be recognised.

### Kirigami

- **`outcome_text`** gains a label for `decided_by_default` rows. It is
  the list's verdict label (flat and grouped) and the inspector's Verdict
  line, with no QML change:
  - "Denied (the firewall's default action)";
  - "Usually allowed (the firewall's default action)". Tower's r8 saw
    requeued packets dropped under nftables chain churn whatever the
    verdict. A marked event after an Ask timeout is exactly that requeue,
    and the bridge can't tell it from the slot-busy case. (Briefly
    "Allowed", then reversed after review: the r8 requeue caveat.)
- **The flag always means "decided, no rule"** (PR #108 review):
  - `is_pending` is false and `Verdict::of` is Other for a flagged row even
    without an action;
  - the `matchedRule` role (`matched_rule_name`) is empty for it whatever
    `matched_rule` says, so "Show rule" and the 5-minute note can't read a
    stray name.
- **`matched_rule_display`** checks the flag first:
  - "No rule: the firewall's default action (deny)" / "(allow)";
  - it never blanks, and never infers this from `matched_rule == None`.
- **No action that assumes a named rule.** "Show rule" needs a non-empty
  `matchedRule`, which a marked row never has.
- **"Make a rule…" is offered, as for a deferred row** (orchestrator
  decision, 2026-10-08): both are connections the default decided.
  - One predicate, `make_rule::offers_make_rule` (deferred **or**
    `decided_by_default`), gates both the Rust `add_rule_message` and the
    sheet's visibility. All of #98's checks stay: remembered durations
    only, a bindable program only, a rule name of its own.
  - The model's `deferred` role, whose only use was that visibility,
    becomes `makeRuleOffered`, computed by the same predicate.
  - The 5-minute-block note needs a `matchedRule`, so it never shows here.
  - Rule-matched and plain decided rows still get no "Make a rule…", and
    neither do pending rows.
- **"Make a rule…" says only what the bridge answered** (PR #108 security
  review, M1; it predates E3, which widens who reaches it).
  - The `AddRule` carries a request id. `MakeRuleController`
    (`make_rule::MakeRuleWait`) waits for its `RuleCommandResult`, as the
    rule editor does.
  - "The rule was created." shows only on Ok. A refusal shows its reason
    (the TCP transport, a name clash, the rule policy, a daemon error), and
    no answer within `NO_ANSWER_AFTER` says the outcome is unknown. All
    plain text.
  - One request at a time; the status shows only on the row it is about.
    Deferred rows share the path.
- **The two-rows hint.** A put-off row without a rule on record
  (`outcome::may_be_listed_again`) gets one fixed plain-text line in its
  inspector: "The firewall may also list this connection, and its retries,
  separately as decided by its default action." It doesn't show on a
  "Decide later" 5-minute block, which a rule decided.

## Known limitations

- **Two rows for one connection** (see above): not folded in v1.
- **Cache eviction.** The bridge's connection cache is bounded and evicts
  the oldest decided rows first, deferred ones included. A burst or flood
  of default-applied connections, which any unprivileged process can cause
  (PR #108 security review, L3), can push other rows, deferred ones
  included, out before the user acts on them. Coalescing marked events is
  a follow-up.
- **The daemon's own cap.** A marked event dropped at the daemon's
  `MaxEvents` cap never reaches the bridge, so that connection isn't
  listed. For the hit counts that is correct: it decides no rule's count.
- **Pings of only marked events.** The daemon pings only when
  `Serialize()` has new events. r12 should confirm on tower's VM that
  pings carrying only marked events reach the bridge; the fork must set
  `newEvents` when it appends one.

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
4. **Kirigami unit** (`connections/outcome.rs`, `connections/row_store.rs`,
   `make_rule.rs`, `bridge_feed.rs`): the labels above; the flag wins over
   `matched_rule` (`is_pending`, `Verdict::of`, `matched_rule_name`);
   `offers_make_rule` and `add_rule_message` accept a default-decided row
   with the same refusals, and refuse a rule-matched one; a fork event's
   row (`1:event-…`) names its session and gets the program-bound
   `AddRule`; `MakeRuleWait`'s Ok, refused and no-reply paths;
   `may_be_listed_again`.
5. **Kirigami QML probe** (`tests/default_action_rows_qml.rs`): a real
   `ConnectionsPage` + `ConnectionsModel` with default-decided rows:
   - the list label, flat and grouped;
   - the inspector's Verdict and Matched rule;
   - no "Show rule", even with a stray `matchedRule`; "Make a rule…" shown
     for default-decided rows, with no block note, and hidden on a
     rule-matched and an unmarked `""` row;
   - the sheet's result: unsent, sending, refused, created, and nothing on
     another row;
   - no two-rows hint here; `deferred_rows_qml.rs` checks it on a put-off
     row and not on a 5-minute block.
   - Guards (`honest_ui_qml_guards.rs`): the hint is one fixed PlainText
     line, and `MakeRuleSheet.qml` never says created or sent itself.
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
- `outcome_text` and `matched_rule_display` ignoring the flag;
- `offers_make_rule` ignoring `decided_by_default`, and the
  `makeRuleOffered` role bypassing it;
- the default-decided allow label losing "Usually";
- `is_pending`, `Verdict::of` and the `matchedRule` role ignoring the flag;
- "Make a rule…" showing success whatever the outcome.

## Gates

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D
warnings`; `cargo test -p snitchwatch-bridge`; `mock_opensnitchd` tests;
Kirigami tests headless (`QT_QPA_PLATFORM=offscreen
QT_QUICK_CONTROLS_STYLE=Basic`); `cargo test` for default members.

## Decisions taken (orchestrator, 2026-10-08; for the owner to review)

- **Folding a marked event into the bridge's own deferred row: not in
  v1.** Both rows stay. Doing it later needs the Ask's source port and pid
  kept on the bridge side, so the bridge can recognise an exactly matching
  event and fill in the deferred row's action (useful when the daemon
  config was unreadable).
- **"Make a rule…" on a default-decided row: yes,** through the deferred
  row's path and checks (see Kirigami).
- **Wording: "Usually allowed (the firewall's default action)"** for a
  default-decided allow. First decided as "Allowed"; **reversed after
  review: r8 requeue caveat** (see Kirigami).
