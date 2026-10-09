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

`expected = rules.len() + left_out.len()`, and the daemon may hold up to
`allowance` more (see "Allowance"): a count in `expected ..= expected +
allowance` agrees.

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
| Prompt answer under a listed name, stored as `<name>-N` | yes, one more | `<name>` replaced | **allowance +1** per answer | `setUniqueName`; typical after the user turned a remembered rule off and the program asked again. Persists until the next snapshot |
| Add the daemon refused after storing it (`Save` failed, or `scheduleTemporaryRule` failed on a duration Go can't parse) | yes | not listed (`RefusedEffect::Unknown`) | **allowance +1** per name | `Replace` stores before `Save`; cleared when the name is listed or by a snapshot |
| Temporary rule the bridge pruned before the daemon's timer fired | yes, until that timer | removed at the wall-clock expiry | **allowance +1** until the timer's monotonic end | Go's timers run on the monotonic clock, which stops while the host is suspended; the wall clock does not |
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

`RulesCache` gets a `CountWatch` and a `MayHold` (the allowance):

- `seen_revision`: the cache `revision` at the last reading. **Every** change
  of the list bumps it (a confirmed or refused rule command, a remembered
  prompt verdict, a pruned expiry, an adopted snapshot, a withdrawal). A new
  revision means the bridge changed its own list since the last reading, or
  adopted a new baseline. That covers "our own confirmed rule command /
  snapshot adoption / reconnect" without hooking each place.
- `quiet`: pings still to ignore. Set to `QUIET_PINGS` when the revision
  changed or the daemon's `uptime` went backwards (a restarted daemon).
- `run` and `key`: how many pings in a row repeated the same
  `(reported, expected, allowance)` triple.
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
5. Pauses: stop and `run = 0`; a hint already on stays on.
   - **Both ways** (nothing counts): a fresh unadopted staged snapshot, and a
     rule command still waiting for the daemon's reply
     (`DaemonCommands::in_flight`, which includes commands that timed out
     within the 30 s late-reply grace). The quiet readings cover the time
     after a reply; this covers the gap between the daemon applying a command
     and its `OK` arriving, however slow, and a reconcile or import burst.
   - **Never raise** (agreeing readings still clear a hint that is on): any
     cached rule with a temporary duration, enabled or not, or a left-out rule
     that was temporary. A rule whose expiry the bridge can't know (a
     duration it can't parse, or a disabled rule whose duration changed) must
     not keep a hint up for good.
6. Compare: `reported` agrees when it is in `expected ..= expected +
   allowance` (at the reading's monotonic time). `key == (reported, expected,
   allowance)` makes `run += 1`, otherwise `key = …` and `run = 1`.
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

### Allowance

Independent review of PR #124 found ordinary in-app sequences that leave the
daemon one rule ahead of the list for good, and so raised a hint that only a
reconnect cleared. The allowance (`MayHold`) is the number of rules the daemon
**may** hold that the list does not show. It only widens the agreeing range
upwards, never downwards; a missed hint is the cheaper error.

1. **A prompt answered under a listed name.** The daemon stores a prompt
   answer through `Loader.Add` (`main.go`, after `Ask`) → `addUserRule` →
   `setUniqueName` (`loader.go`): `<name>-2` (then `-3`, ...) when `<name>` is
   in memory. A rule the user turned off stays in memory, and only enabled
   rules match, so the same program asks again; the answer gets the same name
   (`rule_name_for`), the bridge replaces `<name>`, and the daemon holds one
   more. "For 5 minutes" is the same, with the bridge pruning `<name>` at the
   expiry while the daemon keeps the disabled original. `RulesSync::upsert`
   (the `ask_rule` path) calls `note_prompt_answer(name)`: `renamed += 1` if
   `name` is listed. It persists until the next snapshot.
2. **A refused add.** `Replace` → `replaceUserRule` stores the rule in memory
   before `Save` can fail, and `scheduleTemporaryRule` fails on a duration
   `time.ParseDuration` can't read after the rule is stored; both answer
   `ERROR`. `RefusedEffect::Unknown` leaves the cache as it was, so
   `apply_refused` notes each name of the command that isn't listed
   (`refused`, a set of at most 256). A listed name (an update) adds nothing.
   A later confirmed `CHANGE_RULE` that lists the name drops the entry.
3. **A temporary rule pruned early.** The bridge prunes by the wall clock
   (`now_secs`); the daemon's `time.AfterFunc` runs on the monotonic clock
   (`CLOCK_MONOTONIC` on Linux), which does not count suspend. Each `Expiry`
   keeps `ends`, the daemon timer's monotonic end: the `Instant` when it was
   made plus the **whole** duration. For a change the bridge made that is
   exact. For a snapshot rule it is an upper bound: its `created` stamp is
   old when the daemon loaded it from a temporary-rule file at start (the
   timer starts at load, `loadRule`) or the list was adopted after the host
   slept, so the wall-clock expiry can be long past while the daemon's timer
   has its whole duration to run. The rule rests on Rust's `Instant` and Go's
   runtime timers reading the same clock, `CLOCK_MONOTONIC` and not
   `CLOCK_BOOTTIME`; if either counted suspend time, the tolerance would
   silently stop working. `prune_expired_at(now_secs, clock)` notes
   `ends + 5 s` as an allowance when `ends` is still ahead of `clock`, i.e.
   when the two clocks disagree; in the normal case it notes nothing.

`note_prompt_answer` counts a name that is listed, left out of the list for
its size, or an add the daemon refused and may have stored: the daemon has
the name in each case, and saves the answer as `<name>-2`.

All three clear with `replace_all` (an adopted snapshot) and `set_unknown`.
**The prompt-answer count is never reduced until the next snapshot**, even if
the extra rule later expires or is deleted from outside: it can hide an
outside change of that size for as long as the daemon stays connected (days,
bounded by the number of answers). That is the cheaper error.
Not covered, on purpose: a command that never got an answer (after its 30 s
late-reply grace) is a real divergence the hint may report.

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

**`rules_count_tests.rs`** (27 tests, pure watch plus cache) and
**`rules_count_allowance_tests.rs`** (17: the allowance, clearing while paused,
and the watch driven through adopt and re-adopt on the Unix socket):

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

**Allowance e2e** (`rules_count_hint_unseen.rs`; the shared helpers are in
`tests/count_hint_support/`). `LoaderModel::add_prompt_answer` models
`Loader.Add` (`setUniqueName`, `Save`) and `LoaderModel::change` now stores a
rule before `scheduleTemporaryRule` refuses a duration Go can't parse.
- the user answers `Always`, turns the rule off from the Rules page, the same
  program asks again, the user answers `Always`: the daemon holds `<name>-2`,
  the list shows `<name>` once, and 40 pings never raise the hint; one more
  unseen file on top of it still does (the allowance is one rule, not a
  blanket); the same with "For 5 minutes";
- a recommended-rule install the daemon refuses after storing it (stuck file;
  over TCP the bridge sends no `AddRule`, #35, and the install is the same
  `CHANGE_RULE`): no hint; after the retry succeeds the allowance for it is
  gone and a file the bridge never heard of is said again.
- Not end to end: the suspend sequence (needs a host whose wall and
  monotonic clocks diverge; it is tested on the cache and on `RulesSync` with
  injected clocks), and an add with a duration Go can't parse (the bridge
  sends no `AddRule` over TCP; the cache and the loader model are tested).

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

## As built (differences from the plan above)

- `DaemonCommands::in_flight()` counts waiting commands **and** commands that
  timed out within `LATE_REPLY_GRACE` (30 s): their late `OK` is still
  applied to the cache, so the daemon may have changed what the bridge does
  not know yet.
- The bridge-cli end-to-end tests use `DeleteRule` as the "GUI-made change":
  the per-user setup refuses `AddRule`, and a delete moves the daemon's count
  just the same. The mock gained `LoaderModel::num_rules()` and
  `MockOpensnitchd::ping_reporting_rules()`.
- Kirigami shows the hint only for a `RulesNotShown` that is `listed`
  (`count_hint_text`), whatever else a frame says.
- No snapshot storm: a feed asks for a snapshot only after it subscribes or
  lags (`bridge_dispatch::run_feed`). The hint adds at most one small
  `RulesNotShown` per flip, never per ping, and nothing in this path sends a
  command or a list.
- No role id was added to `RulesModel` (the hint is page state, a
  `countHintText` property beside `notShownText`).

## Mutation checks (as run)

38 one-line mutants, each applied alone and run against the new tests:
thresholds (raise 1 and 2, clear 1 and 2, quiet 0 and 1), `expected` without
`left_out` and with `files_left`, each pause dropped (temporary rules, disabled
temporary rules, left-out temporary rules, in-flight commands, staged
snapshot, zero reading, the ping handler's in-flight argument and its whole
call), no restart detection, runs that survive a pause or a changed count, a
broadcast per ping, a flag missing from `not_shown`, resets missing from
`replace_all` and `set_unknown`, the staged-snapshot and late-reply bounds, the
wire field always serialized, and in Kirigami a non-plain label, a header that
ignores the hint, an always-visible label, the `listed` guard, the model never
setting the text and a hint with no remedy. Two survived the first run (a quiet
of 1, and an always-visible label inside an already visible header) and got
tests that pin them; all 38 are killed.

### Mutation checks, review fix round (private copy, never in place)

Run in a copy of the tree under the scratchpad, with the worktree's target
directory. 23 new mutants for the allowance and the pauses: each allowance
dropped, applied to a new name, to a listed refused name, never forgotten
(snapshot, withdrawal, upsert, confirmed delete), capped off by one; the
pruned-rule allowance never noted, always noted, never ending, on the wrong
clock or with `ends` = now; a range that accepts a count below the list or
has no allowance; timers pausing both ways or allowing a raise; a pause that
keeps the run; the ask path not noting the collision. The three end-to-end
claims were each checked against the end-to-end tests too (the prompt
allowance, the refused install, the forgetting). One survived (the
withdrawal's own clear is redundant with `replace_all`'s) and got a direct
assertion. The 31 mutants of the first round that still apply were re-run
against the new code: all killed. The mock's `add_prompt_answer` and
duration handling have unit tests of their own.

## Limitations (also for the tower gate)

- **The allowance is a blind spot, by design.** After one of the three cases
  above, a file added outside Snitchwatch is not said until it is one more
  than the allowance, or until the next snapshot (a daemon reconnect).
- **A second adoption on the Unix socket replaces the list** with the staged
  snapshot and drops what was confirmed in between (`RulesSync::readopt`,
  PR #106 N1). The daemon's count then really differs from the list's, and the
  hint says so after the quiet readings, though its wording guesses the wrong
  cause. The watch resets at each adoption (a unit test drives adopt and
  re-adopt).
- **A forged `Ping` on the legacy TCP port (#35)** can raise or clear the
  hint. It can already send fake statistics and prompts there; the hint
  triggers no re-send, snapshot request or command.
- **Open question, not fixed here:** `PendingReply::drop` removes the waiter
  without moving the command to `late`, so `in_flight()` reads 0 and a later
  `OK` is ignored. No live path drops a handle before `wait()` (every sender
  awaits it), but with the hint such a command would be a permanent
  disagreement.
- Go's timers use the runtime's monotonic clock, `CLOCK_MONOTONIC` on Linux,
  which stops while the host is suspended; the allowance for a pruned
  temporary rule relies on that (reasoned from Go's design, not read from the
  runtime source in this repository).

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
