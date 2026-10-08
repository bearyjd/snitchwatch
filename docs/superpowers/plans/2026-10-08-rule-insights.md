# Rule insights: hit counts, unused and shadowed rules, a full simulator (roadmap P2.6)

**Date:** 2026-10-08
**Roadmap:** P2.6 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`
**Baseline:** `main` @ `d9d1bfe` plus #48.
**Blocked on:**
- #48, for Parts 1 and 2. Hits are only useful against the full rule
  list, and pruning keys by cached names.
- Nothing, for Part 3 (the simulator, Kirigami-only). It uses #48's
  `Rule.precedence` once that merges.

**Size:** M, as three PRs:
1. Bridge hit counts and the protocol (S–M).
2. Kirigami unused and shadowed analysis (S–M).
3. Simulator for all operands (S–M).

## Citation convention

- `main:` means `d9d1bfe`.
- `#48:` means branch `fix/48-show-all-rules` @ `ebdd21d` (about to merge;
  re-check names after it does).
- `vendor:` means opensnitch v1.8.0.

Functions are cited by name.

## Goal

1. **Per-rule hit counts and last-hit time,** derived from
   `Statistics.events`. They are labelled "counted by Snitchwatch since
   <time>" and "approximate".
2. **"Unused" rules:** enabled, counted, and with no hits for the
   observation window.
3. **"Never applies" and "redundant" rules,** from static analysis of the
   cached rule list under the daemon's real evaluation order. Claims are
   made only where they can be proven.
4. **A simulator** that evaluates every operand opensnitchd v1.8.0
   evaluates, or says exactly why it can't.

## Out of scope

- Persistent history (P3.1) and byte counts (P3.4).
- Deleting rules automatically, or a bulk "delete unused". Per-row delete
  already exists.
- `lists.*` contents in the simulator. Even after #45 PR B, only
  Snitchwatch's own list directory could be read.
- A global per-rule "matched" count. The daemon names only the
  *deciding* rule (see Findings).

## Findings

**Events** (`vendor:daemon/statistics/stats.go`):
- `onConnection` appends an `Event { connection, rule }` **only for
  matched connections** (`wasMissed == false`).
  - The rule is the **deciding** rule from `FindFirstMatch`, or the
    reply rule of an answered prompt. For a once-only answer, that is the
    bridge's synthetic name, which is not in the rules list.
- **Batch size.** Events are capped at `maxEvents`, dropping the oldest:
  250 from `Stats.MaxEvents` in both the vendor and the packaging
  `default-config.json`, and 150 as the code default.
- **Events are a per-ping batch.**
  - `Serialize` empties them (`defer emptyStats`) *before* knowing
    whether the Ping RPC succeeds.
  - `client.go` `poller` pings about once a second, and skips the ping
    when `Serialize` returns nil.
  - Counts are therefore lost on bursts of more than `MaxEvents` per
    interval, on a failed ping, and whenever no bridge is connected.
- **`nolog` rules never produce events.** `vendor:daemon/main.go`
  `onPacket` returns on `r != nil && r.Nolog` before
  `stats.OnConnectionEvent`, so such a rule always shows 0 hits.
- **Only global counters exist on the daemon side:** `rule_hits` /
  `rule_misses` (relayed as `ServerMessage::DaemonStatistics`).

**Bridge (`main`):**
- The `grpc_server.rs` `ping` handler already iterates `stats.events`.
  Each becomes a decided row through `translator/connection.rs`
  `event_to_row`, which returns `None` when `event.rule` is absent.
- **Daemon configuration.** `ClientConfig.config` (a JSON string) carries
  `Stats.MaxEvents` and `Rules.EnableChecksums`. The bridge doesn't read
  it today. Doc 2 (`2026-10-08-prompt-slot-ux.md`, step 10) introduces
  `daemon_config.rs`; whichever plan lands first owns it.

**Kirigami simulator** (`rules/simulator.rs`):
- It evaluates the store in order (the daemon sorts enabled rules by
  name), with deny/precedence stopping the scan and otherwise the last
  matching allow winning. That mirrors `vendor:daemon/rule/loader.go`
  `FindFirstMatch`.
- **Supported operands:** `SUPPORTED_OPERANDS = process.path, dest.host,
  dest.port, protocol`, plus `list` and `true`. Everything else is
  "unsupported → non-matching".
- **Precedence is stubbed.** `is_precedence` returns `false`, because the
  Kirigami `Rule` had no `precedence`. #48 adds `Rule.precedence`
  (`rules/row_store.rs`), so the stub can go.
- `normalized_action` folds `reject` to `deny`. That is correct for
  evaluation, since reject also stops the scan.
- **Inputs.** `SimulationInput` has only `{process_path, dest_host,
  dest_port, protocol}`.
- **Prefill.** `ConnectionRow` carries only `process_path`, `dst_host`,
  `dst_ip`, `dst_port` and `protocol`. Args, env and checksums are dropped
  (P3.3).

**Operand semantics** (`vendor:daemon/rule/operator.go` `Match` /
`Compile`; `simple` compares with `EqualFold` unless `sensitive`;
`regexp` lowercases both the pattern and the subject unless `sensitive`):

| Operand | The daemon compares | Simulator note |
|---|---|---|
| `process.path` | `Process.Path` | supported |
| `process.parent.path` | **any** ancestor's path (walks `Parent`) | needs a list of ancestors |
| `process.command` | args joined with `" "` | input field |
| `process.id` | pid, decimal | input field |
| `process.env.NAME` | that variable, `""` if unset | input map |
| `process.hash.md5`/`sha1` | **always true when checksums are off**; an empty hash also matches (`hashCmp`) | mirrors the daemon, with a loud warning |
| `user.id` | uid, decimal | input field |
| `user.name` | resolved to a uid **at compile time on the daemon host** (`user.Lookup`) | unsupported, with that reason |
| `source.ip` / `source.port` / `dest.ip` / `dest.port` | string compare | input fields |
| `dest.host` | `DstHost` (`""` for a bare IP) | supported |
| `dest.network` / `source.network` | type `network`: a CIDR, or an alias (`LAN`, `MULTICAST`) | CIDR math; aliases embedded from `vendor:daemon/data/network_aliases.json` |
| `protocol` | `con.Protocol` | supported |
| `iface.in` / `iface.out` | interface name by index; false if the lookup fails | input fields |
| `lists.*` | files loaded from a directory | unsupported, with that reason |

## Design

### Part 1: hit counts (bridge, after #48)

1. **`RuleHits`** (new file `crates/snitchwatch-bridge/src/cache/rule_hits.rs`):
   - **State:**
     `BTreeMap<String, HitStat { count: u64, last_hit_unix_ms: i64 }>`
     plus `since_unix_ms` (the first ping that carried stats) and
     `lossy: bool`.
   - **`record(events)`** is called from the `ping` handler, next to the
     existing `event_to_row` loop. For each event with a rule, it
     increments that rule name's stat; `last_hit` is from
     `Event.unixnano`.
   - **Names outside the cache.**
     - While the `RulesCache` (#48) is `Synced`, an event whose rule is in
       the cache counts in the main map.
     - Anything else goes into a bounded side map of at most 1 000
       entries: a once-reply synthetic name, a rule the cache doesn't know
       yet, or any event while the cache is `Unknown`.
     - At the next snapshot commit, side-map entries whose names are in
       the snapshot move to the main map. The rest are discarded.
   - **Pruning happens only on these two signals:**
     - a new `Synced` snapshot is committed (#48 `RulesSync::commit`):
       drop main-map names that aren't in it;
     - a confirmed `DELETE_RULE` (#48 `apply_confirmed`): drop that name.

     **Never prune on `RulesSync::withdraw`.** It sets the cache to
     `Unknown` and publishes an empty `SetRules` every time the daemon
     stream closes, which happens on every daemon restart or reconnect.
     Pruning on it would wipe all counts.
   - **`lossy`** is set when a batch length reaches `max_events` (from
     `daemon_config`, else 150). It never clears for the session.
   - **Bounds.** At most `MAX_SNAPSHOT_RULES` entries in the main map. A
     rename starts at zero.
   - **Persistence** is owner question N1. If yes, save a JSON file in
     the bridge state directory (#45 PR A's resolver) every 5 min and on
     shutdown. Load it at start, but only for names that match the first
     committed snapshot.
2. **Protocol** (additive):
   - `ServerMessage::RuleHits { since_unix_ms, lossy, hits: Vec<RuleHitWire { name, count, last_hit_unix_ms }> }`,
     broadcast at most every 5 s and only when something changed;
   - included in the `RequestSnapshot` answer.

### Part 2: unused and shadowed (Kirigami, Qt-free `rules/insights.rs`)

3. **Hits in the model.** `RulesStore` takes `RuleHits`. `RulesModel`
   gains the roles `hitCount`, `lastHitMs` and `hitsCounted`, the last
   false for `nolog` rules ("Not counted: this rule doesn't log").
   `RulesPage.qml` gets "Hits" and "Last used" columns, with a tooltip
   "Counted by Snitchwatch since <time>; approximate" plus "may be
   missing some" when `lossy`.
4. **Unused:** `unused(store, hits, now, window) -> Vec<name>`. A rule
   qualifies when it is:
   - enabled;
   - not `nolog`;
   - of duration `always` or `until restart`;
   - observed for at least `window` (owner N2);
   - with a count of 0.

   Without persistence (N1 = no), the badge reads "No hits since <time>"
   and never "Unused".
5. **Shadowed and redundant** (static; hit counts can't show this, since
   only the deciding rule is ever counted):
   - **Normalise** each enabled rule's operator into a conjunction of
     atoms (`list` = AND, nested lists flattened).
     - Anything unmodelled is **opaque**: `lists.*`, `process.hash.*`,
       `user.name`, `iface.*`, `process.env.*`, an unknown type. A rule
       with an opaque atom is never claimed as covering another.
   - **`covers(a, b)`:** every atom of A is implied by some atom of B.
     Atom implication is conservative and needs the same operand:
     - **simple/simple:**
       - when A is insensitive, the data are equal under ASCII case
         folding;
       - when A is sensitive, B must be sensitive too and the data
         exactly equal;
     - **A regexp, B simple:** A matches B's literal, and A is
       insensitive or B is sensitive;
     - **A `true`:** always;
     - **A network, B literal `dest.ip`** or a narrower CIDR: containment.
     - Everything else: not implied.
   - **Decisive semantics** (`FindFirstMatch`):
     - A *stop rule* is deny, reject or `precedence`.
     - A non-stop allow **B** never decides when some stop rule A, at any
       position, covers it, or when some later non-stop allow A covers
       it.
     - A stop rule **B** never decides when an *earlier* stop rule A
       covers it.
   - **Findings:**
     - "Redundant: <A> already decides these connections the same way"
       when the actions match;
     - "Never applies: <A> decides first" when they differ.

     Each finding names A and links to its row.
   - **Cost.** O(n²) over enabled rules. Run it on demand ("Analyze
     rules" button) on a worker thread, and cap it at 2 000 enabled rules
     with a "too many rules to analyze" message. #48 allows 10 000 rules.

### Part 3: the simulator for every operand (Kirigami, Qt-free)

6. **`SimulationInput`** gains:
   - `parent_paths: Vec<String>`, `command: String`, `pid: Option<u32>`;
   - `uid: Option<u32>`, `env: BTreeMap<String, String>`;
   - `src_ip`, `src_port`, `dest_ip`;
   - `iface_in`, `iface_out`;
   - `checksums: BTreeMap<String, String>`, `checksums_enabled: Option<bool>`.

   **Optional means unknown, not empty.** An operand whose subject is
   unknown is reported as *unevaluated* instead of being guessed, and the
   result says which inputs it lacked.
7. **Operand evaluation** per the table in Findings.
   - **Regexp** lowercases the pattern and the subject when not
     sensitive.
   - **`network` / CIDR** uses `std::net` plus prefix math (no new
     crate). Aliases are embedded, with a test pinning them to the
     vendored JSON.
   - **Hash operands with `checksums_enabled` false or unknown** match,
     as the daemon does. The result carries "Hash conditions match every
     program while checksums are off" (or "…may…" when unknown).
   - **`user.name` and `lists.*`** stay unsupported, each with its own
     reason string.
8. **Fix `is_precedence`** to read #48's `Rule.precedence`.
9. **UI.**
   - The simulator panel on `RulesPage.qml` gets an "Advanced inputs"
     section.
   - "Simulate this connection" from the Connections inspector prefills
     what `ConnectionRow` carries and marks the rest unknown.
   - Every result keeps the existing "simulation, not a live verdict"
     label.

## Tests to write first

**Part 1** (`rule_hits.rs`, `grpc_server/tests.rs`):
- **Counting.** Three events for rule `a` and one for `b` → `a:3`,
  `b:1`; `last_hit` comes from `unixnano`.
- **An event without a rule** is ignored, matching `event_to_row`.
- **A once-reply synthetic name** never appears in `RuleHits` after the
  next snapshot commit.
- **Before the cache is synced,** counts are held in the side map. They
  are adopted at the commit whose snapshot contains the name.
- **`lossy`.** A batch of exactly `max_events` sets it. A batch of
  `max_events - 1` doesn't.
- **Counts survive a reconnect.** `a:3`, then `RulesSync::withdraw`
  (cache `Unknown`, empty `SetRules`), then a re-commit containing `a`:
  `a` is still 3, and hits during the `Unknown` gap are added on adoption.
- **A re-commit that lacks `b`** drops `b`.
- **A confirmed `DELETE_RULE`** drops its entry. A rejected one doesn't.
- **Rate limit.** 50 pings within 5 s produce at most two `RuleHits`
  broadcasts (`tokio::test(start_paused = true)`).
- **Snapshot.** `RequestSnapshot` includes `RuleHits`.

**Part 2** (`rules/insights.rs`):
- **`unused` table:**
  - `nolog` → excluded;
  - disabled → excluded;
  - `"5m"` → excluded;
  - observed for less than the window → excluded;
  - 0 hits over the window → included.
- **`covers`:**
  - an insensitive simple covers a case variant; a sensitive one doesn't
    cover an insensitive one;
  - a regexp `^.*\.example\.com$` covers the simple `a.example.com`;
  - `true` covers anything;
  - `10.0.0.0/8` covers the `dest.ip` `10.1.2.3` and the network
    `10.1.0.0/16`;
  - an opaque atom never covers.
- **Decisive semantics:**
  - a deny at a *later* position still shadows an earlier allow;
  - a later allow shadows an earlier allow;
  - an earlier allow does *not* shadow a later allow;
  - a precedence allow before a deny shadows that deny;
  - a deny after a precedence allow does not shadow it.
- **The 2 001-rule cap** returns the "too many" result.

**Part 3** (`rules/simulator.rs`):
- one positive and one negative case per operand in the table;
- `parent.path` matches a grandparent;
- `process.command` joins args with spaces;
- `process.env.HOME` unset compares as `""`;
- **hash:** checksums off → match, with the warning; on → compares;
  unknown → match, with "may";
- `user.name` → unsupported with its reason;
- an unknown input → unevaluated, never a match;
- **precedence:** a precedence allow stops the scan before a later deny
  (this test fails today, because of the stub).

## Verification

Run at low priority (`nice -n 19`):
- `cargo test -p snitchwatch-bridge rule_hits`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-kirigami rules`, with
  `QT_QPA_PLATFORM=offscreen`
- `cargo clippy --all-targets -- -D warnings`

Tower VM checks:
1. **Hit counts.** Generate traffic matching one rule 20 times. "Hits"
   shows about 20 and "Last used" is current.
2. **`nolog`.** A `nolog` rule shows "Not counted".
3. **Shadowing.** Add a host-wide deny for `example.com` and an app-bound
   allow for curl → `example.com`. The allow is reported "Never applies"
   because of the deny, and `curl` is indeed blocked.

## Risks

- **Counts are lossy.** Bursts, failed pings and bridge downtime all lose
  hits; the `lossy` flag catches only the first. Wording stays
  "approximate".
- **"Unused" without persistence is a session fact.** That is why N1/N2
  gate the badge.
- **Static analysis is incomplete by design.** No finding doesn't mean no
  shadowing. The panel says "Snitchwatch checks only conditions it can
  compare exactly."
- **Regex dialects differ** (Go RE2 vs the Rust `regex` crate) for rare
  constructs. The simulator labels its results as simulations.
- **File-conflict hot spots:**
  - `grpc_server.rs` `ping` (doc 2 reads `rule_misses` there);
  - `subscribe` (`daemon_config`);
  - `ws_messages.rs`;
  - bridge-cli `SnapshotRequested`;
  - `rules/simulator.rs`, `rules/row_store.rs`, `rules_model.rs` and
    `RulesPage.qml`, with #44 Part B, P2.7 and P2.1.

## OWNER QUESTIONS

- **N1. Persist hit counts across bridge restarts** before P3.1 lands?
  The cost is a small JSON file in the bridge state directory, after #45
  PR A adds the resolver. The benefit is that "unused" can mean more than
  "since this boot". **Recommendation: yes.**
- **N2. The "unused" window.** Options: 7, 14 or 30 days with zero
  counted hits. **Recommendation: 14 days,** shown only when N1 = yes;
  otherwise the badge is only "No hits since <time>".
