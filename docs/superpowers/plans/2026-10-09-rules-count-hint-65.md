# Rules page hint when the firewall's rule count disagrees (issue #65, option c)

**Date:** 2026-10-09
**Issue:** #65 stays **open**. This is a partial mitigation, chosen by the
owner in #117 (option c). A full fix needs a daemon "rules reloaded" push, a
new bazzite-tower daemon change, which is out of scope.
**Baseline:** `main` @ `ac446e2`, branch `feat/65-rules-count-hint`.
**Size:** M. Bridge (rules cache, ping handler), one additive wire field,
`mock_opensnitchd` (two small helpers), Kirigami (one string, one label).

## Citation convention

- `vendor:` means `vendor/opensnitch` v1.8.0 (read-only), cited by function.
- `main:` means `ac446e2`.

## Goal

When opensnitchd's own file watcher loads or drops a rule file, the Rules
page does not follow (#65): the bridge's list comes only from `Subscribe`'s
`ClientConfig.rules` and from changes the bridge itself makes. The daemon
does tell the bridge how many rules it holds, in every `Ping` that carries
statistics. This plan turns a **persistent** disagreement between that number
and the bridge's list into one fixed, plain-text hint on the Rules page.

The hint is advice, not a refresh. It never re-sends anything, requests
nothing from the daemon, and closes no stream.

## Out of scope

- Refreshing the list. (a) re-sending the cache would show nothing new;
  (b) closing the daemon's stream drops prompts for about a second.
- A daemon "rules reloaded" push (the real fix, tower).
- Detecting an **in-place edit** of a rule file. See Limitations.
- Hiding the hint per user, or showing the two numbers.

## What the daemon reports (evidence)

| Fact | Evidence |
|---|---|
| `DaemonStatistics.rules` is `Statistics.rules`, set to `s.rules.NumRules()` when the ping is built | `vendor:daemon/statistics/stats.go` `Serialize` |
| `NumRules()` is `len(l.rules)`: **every** loaded rule, enabled or not | `vendor:daemon/rule/loader.go` `NumRules` |
| `Subscribe`'s snapshot is `len(c.rules.GetAll())` rules: the same map | `vendor:daemon/ui/notifications.go` `getClientConfig`, `loader.go` `GetAll` |
| A disabled rule is stored | `loader.go` `loadRule` (the `!r.Enabled` branch stores it) |
| A rule that fails to parse or compile is **not** stored | `loadRule` returns before `l.rules[r.Name] = &r` |
| A `once` verdict is never stored | `addUserRule` returns for `Once` |
| A temporary rule leaves the map when its timer fires | `scheduleTemporaryRule` (`time.ParseDuration`, `delete(l.rules, …)`) |
| A prompt answer whose name exists is stored as `<name>-2` | `setUniqueName` |
| The watcher handles only `Write` and `Remove` events of `*.json` files | `liveReloadWorker` |
| A new or rewritten file is loaded by name from its **content** | `loadRule` (`l.rules[r.Name]`) |
| A removed file drops its rule only when the rule is `always` and the **file name** is the rule's name | `deleteRule` |
| `Delete` removes from memory first, then the file | `Delete` (`delete(l.rules, …)` before `deleteRuleFromDisk`) |
| **No ping is sent when there are no new events** | `stats.go` `Serialize` returns `nil` without new events; `vendor:daemon/ui/client.go` `ping` returns before the RPC |
| Pings are at least about 1 s apart | `client.go` `poller` (`time.Sleep(1 * time.Second)` between iterations) |
| `rules` is a plain proto3 `uint64`: 0 and "not reported" look the same | `vendor:proto/ui.proto` `Statistics.rules` |

So the number can only be compared **on pings that carry statistics**, i.e.
while there is connection activity. On an idle machine the hint is late, not
wrong.

## Count semantics

The comparison is `reported` (from the ping) against `expected`
(from the bridge's `RulesCache`):

`expected = rules.len() + left_out.len()`

| Daemon-side thing | In `NumRules()`? | Bridge side | Counted in `expected` | Notes |
|---|---|---|---|---|
| Enabled rule from a file or a bridge command | yes | cache entry | yes | |
| Disabled rule | yes | cache entry | yes | listed disabled in the GUI |
| Hidden or reserved-prefix rule (`z00-blocklist:`, `900-blocklist:`, `000-snitchwatch-`, `snitchwatch-default-`, `850-profile:`) | yes | cache entry; only Kirigami groups or hides them | yes | the cache holds the whole daemon list, not the GUI's view |
| Rule over the per-field size limits | yes | `left_out` (name and size) | yes, via `left_out.len()` | `RulesNotShown.tooLarge` counts the same entries |
| Snapshot over 10,000 rules | yes | no list (`Unknown`), only a total | **no evaluation** | the hint needs a list |
| No list yet, or withdrawn (stream gone) | n/a | `Unknown` | **no evaluation**; the hint clears | |
| Deleted rule whose file the daemon could not remove (`files_left`) | **no** (left memory first) | removed from the cache | no | `RulesNotShown.leftOnDisk` says it; no double counting |
| `once` verdict | no | not cached | no | agrees |
| File that fails to parse or compile | no | not in the snapshot | no | agrees |
| Staged `Subscribe` snapshot, not adopted yet | yes | not in the cache | evaluation **pauses** while a fresh unadopted snapshot waits (30 s bound) | the next adoption is a new baseline |
| Temporary rule (duration not `always`, `until restart` or `once`), **enabled or not** | yes, until its timer fires | cache entry; an approximate expiry only if it was enabled and parses (30 s prune tick, `\d+[smh]`) | evaluation **pauses** while any is cached | the daemon's timer outlives a later disable (`scheduleTemporaryRule` ignores `enabled`), and a snapshot gives a disabled rule no expiry; the bridge's expiry and the daemon's timer also disagree by seconds, and for durations the bridge cannot parse (`1.5h`) for good |
| Temporary rule among those left out by the size limits | yes | `left_out` stores no duration | evaluation **pauses** (the snapshot notes that one was temporary) | otherwise its timer would leave a permanent −1 |
| Prompt answer stored as `<name>-2` | yes, one more | `<name>` replaced | counted: a real +1 | a true disagreement, the known divergence in `cache/rules.rs`; the hint is honest |
| File added with a new rule name | yes (+1) | unknown | disagrees | **detected** |
| File removed (`always`, file name = rule name) | no (-1) | unknown | disagrees | **detected** |
| File edited in place, or a moved-in file | unchanged | unknown | agrees | **not detected** (a `mv` into the directory raises no `Write` event, so the daemon ignores it too) |

## Design

All state lives in `RulesCache` (under the cache lock, so a reading, the list
and the flag are always seen together). New file
`crates/snitchwatch-bridge/src/cache/rules_count.rs`, a `#[path]` submodule of
`rules.rs` like `rules_refused.rs`, with its tests in
`rules_count_tests.rs`.

### The watch

`RulesCache` gets a `CountWatch`:

- `seen_revision`: the cache `revision` at the last reading. **Every** change
  of the list bumps it (a confirmed or refused rule command, a remembered
  prompt verdict, a pruned expiry, an adopted snapshot, a withdrawal). A new
  revision means the bridge changed its own list since the last reading, or
  adopted a new baseline. That covers "our own confirmed rule command /
  snapshot adoption / reconnect" without hooking each place.
- `quiet`: pings still to ignore. Set to `QUIET_PINGS` when the revision
  changed or the daemon's `uptime` went backwards (a restarted daemon).
- `run` and `key`: how many pings in a row repeated the same
  `(reported, expected)` pair.
- `raised`: whether the hint is on. Only this goes on the wire.

`RulesSync::observe_daemon_rules(reported, uptime, commands_in_flight)` is
called by the ping handler (`grpc_server.rs`) for a ping that has `stats`, as
its own statement: not inside the `new_rows` block, which holds the rules
cache lock (a std mutex, not reentrant), and not inside `record_hits`.
`DaemonCommands::in_flight()` and the staged-snapshot check each take and
release their own lock **before** the cache lock is taken, so no new lock
nesting exists (`DaemonCommands::on_reply` already nests its lock outside the
cache's). It takes the cache lock, asks the watch, and **only when `raised` flips** broadcasts
`cache.not_shown()` (a `RulesNotShown`, never a `SetRules`), still under the
lock like every list publisher.

Per reading, in order:

1. No list (`Unknown`): nothing to compare. (`set_unknown` and `replace_all`
   already cleared the watch; see below.)
2. Revision changed or uptime dropped: `quiet = QUIET_PINGS`, `run = 0`.
3. `quiet > 0`: `quiet -= 1`, stop.
4. A reading of 0 while the list is not empty: stop, `run = 0`. The field
   cannot say "not reported", so 0 is not evidence. (A daemon that really
   lost every rule file goes unflagged; a missed hint is the cheaper error.)
5. Any of these: stop, `run = 0`. A hint already on stays on.
   - a fresh unadopted staged snapshot;
   - any cached rule with a temporary duration, enabled or not, or a left-out
     rule that was temporary;
   - a rule command still waiting for the daemon's reply
     (`DaemonCommands::in_flight`). The quiet readings cover the time after
     a reply; this covers the gap between the daemon applying a command and
     its `OK` arriving, however slow, and a reconcile or import burst.
6. Compare: `key == (reported, expected)` makes `run += 1`, otherwise
   `key = …` and `run = 1`.
7. Off and disagreeing for `PINGS_TO_RAISE` readings in a row: **on**. On and
   agreeing for `PINGS_TO_CLEAR` readings in a row: **off**. Nothing else
   flips it.

Numbers (all pings with statistics, ~1 s apart while busy):

| Constant | Value | Why |
|---|---|---|
| `QUIET_PINGS` | 2 | a ping built before the daemon applied, or the bridge applied, a change is at most one reading old; two covers it with margin |
| `PINGS_TO_RAISE` | 3 | the owner's floor; a disagreement must hold for about 3 s of activity |
| `PINGS_TO_CLEAR` | 3 | same debounce for hysteresis |

Properties that follow:

- A count that keeps moving (an import, many files dropped at once) never
  repeats a key, so the hint waits until the daemon settles.
- Flapping (disagree, agree, disagree) keeps resetting `run`; nothing flips.
- The hint is never a reaction: there is no re-send, no snapshot request and
  no command anywhere in this path. A snapshot is adopted only when the daemon
  reconnects, and adoption resets the watch.

### Resets

- `replace_all` (an adopted snapshot): watch cleared, `raised = false`, so the
  `SetRules` + `RulesNotShown` that follow carry no stale hint.
- `set_unknown` (withdrawal, the stream is gone): same.
- `prune_expired` and every other change only bump the revision (step 2).

## Wire shape

One additive field on the existing `RulesNotShown`:

```json
{ "action": "rulesNotShown", "tooLarge": 0, "listed": true, "countMismatch": true }
```

- `countMismatch: bool`, `#[serde(default, skip_serializing_if = false)]`:
  **omitted when false**, so a bridge with no mismatch sends exactly what it
  sent before.
- An old GUI ignores the unknown field (`ServerMessage` has no
  `deny_unknown_fields`; a test pins that). An old bridge never sends it, so
  a new GUI reads it as false and shows nothing.
- It rides the existing message so it is in every `publish_rules`: a GUI that
  connects later, or sends `RequestSnapshot`, gets the current state.
  A `RulesNotShown` carries no rows, so toggling the hint never touches the
  list and Kirigami does not reset its model for it.
- No capability string: nothing depends on it.

## UI (Kirigami)

`RulesModel` gets a property `countHintText` (a `QString`, "" when off), set
from `RulesNotShown` in the branch that already sets `notShownText`.
`RulesPage.qml` shows one `Controls.Label` (`objectName: "rulesCountHint"`,
`textFormat: Text.PlainText`, `wrapMode: Text.Wrap`) below the "not shown"
label, visible when the text is non-empty. The text is a constant in
`rules/not_shown.rs`; no daemon text is ever rendered:

> The firewall service reports a different number of rules than this list
> shows, so rule files may have been changed outside Snitchwatch. Restarting
> the firewall service refreshes the list.

(No role id is added; the hint is page state, not row data.)

## Tests (TDD, each written to fail first)

**`rules_count_tests.rs`** (pure watch plus cache):

- equal counts never raise; two disagreeing readings do not raise, three do;
  a moving count never raises;
- the first two readings after a revision change (confirmed command, remembered
  verdict, adopted snapshot, pruned expiry) are ignored, a third counts;
- a uptime drop restarts the quiet readings;
- `left_out` counts toward `expected`; a `files_left` marker does not;
- a zero reading is ignored while the list is not empty, and agrees with an
  empty list;
- each pause leaves a raised hint raised and never raises one: a temporary
  rule (enabled; **disabled**, from a snapshot with no expiry, followed by a
  daemon count one lower), a temporary left-out rule, a fresh unadopted
  staged snapshot, and a command in flight;
- clearing needs three agreeing readings; disagree/agree flapping never
  flips it; the broadcast fires **once per flip**, never per ping, and is a
  `RulesNotShown` with no `SetRules`;
- `replace_all` and `set_unknown` clear a raised hint; `publish_rules` carries
  the flag (a late GUI sees it).

**Wire** (`ws_messages` tests): `countMismatch` is omitted when false and
present when true; an old frame without it reads as false; an unknown extra
field is ignored.

**Bridge e2e** (`crates/snitchwatch-bridge-cli/tests/rules_count_hint.rs`,
mock daemon with `LoaderModel` answering commands, as in
`curated_defaults.rs`): the mock reports its loader's `NumRules` in pings.
- agreement for many pings: no hint;
- "a rule file appears": insert into the loader's memory without telling the
  bridge, ping three times: one hint; ping on: still one; remove it: three
  agreeing pings clear it, once;
- a GUI-made rule change: the daemon applies it, the stale reading before the
  reply and the fresh ones after never raise it;
- a daemon reconnect with the new count: adopted, hint cleared, no new one.

**Kirigami**: `not_shown` unit tests (text, wording facts: no daemon text,
mentions restart); a QML probe (`rules_count_hint_qml.rs`, same pattern as
`rules_all_apps_qml.rs`): the label is hidden, shown with `Text.PlainText`
and the fixed text after a mismatch `RulesNotShown`, hidden again when the
next one has none, and a `SetRules` alone does not hide it.

**Mutation checks** (reported): `PINGS_TO_RAISE` 3 to 1; drop the quiet
ping count; compare without `left_out`; drop the temporary-rule pause; make
it ignore disabled temporary rules; drop the in-flight pause; drop the
staged-snapshot pause; drop the zero-reading guard; clear on one agreeing ping; broadcast on every
ping; leave the flag out of `not_shown()`; `replace_all` not clearing it;
Kirigami `Text.PlainText` removed.

## Limitations (also for the tower gate)

- **In-place edits are not detected.** An edited file that keeps its rule
  name replaces the rule and leaves the count alone. So does any change that
  swaps one rule for another.
- Only pings that carry statistics are evaluated, so an idle machine shows
  the hint late. After a quiet period the first busy pings (about 3 s) raise
  it.
- While any enabled temporary rule exists (a "for 5 minutes" answer), the
  check pauses; a hint already shown stays.
- A forged `Ping` on the legacy TCP port (#35) could raise the hint. It can
  already send fake statistics and prompts there; the hint is advice only
  and changes nothing.
- A mismatch whose cause is the bridge's own approximation (a disk-loaded
  temporary rule whose daemon timer started at load, not at `created`) is a
  true disagreement and is flagged until the timer fires.
- The hint says what is known: the numbers differ. It does not say why.

## Verification

- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`;
  `cargo clippy -p snitchwatch-kirigami --all-targets -- -D warnings`
- `cargo test -j 4 --no-fail-fast` and the Kirigami headless suite
- `just package-check`
- Tower r13. The daemon's watcher handles only `Write` and `Remove` events of
  `*.json` files, and pings carry statistics only while there is traffic.
  1. With the patched daemon running, the bridge connected and **no
     temporary rule** present, add a valid rule file with `cp` or `cat >`
     (not `mv`, and not an editor that saves by rename: those raise no
     `Write`, the daemon never loads the file, and the gate would fail for
     the wrong reason). Generate some traffic. The hint appears after about
     three pings, and the new rule is **not** listed.
  2. `rm` that file (an `always` rule whose file name is its rule name; a
     differently named file leaves the rule in memory). The hint clears
     after three pings with traffic.
  3. Add the file again, then restart `opensnitchd`: the list gains the rule
     and the hint is gone.
  4. An in-place edit of an existing file (same rule name) must **not** show
     the hint.
  5. Rule changes made from the GUI, and a "for 5 minutes" answer, must never
     show it; while that temporary rule lasts, step 1 shows nothing either.
