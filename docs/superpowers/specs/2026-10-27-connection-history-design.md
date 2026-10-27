# Connection history (r17): opt-in storage, retention, gaps, event-time rule identity

Status: **PROPOSED, awaiting owner approval.** Design only; no code is
authorized by this document. Implementation waits for the owner to resolve the
[open questions](#open-questions-for-the-owner) and approve.
Baseline: main `c4430bd` (2026-10-27). Revised after adversarial design and
security review (round 1).
Roadmap items: **H01** (opt-in persistent allowed/denied history bounded by
age and disk size), **H02** (clear history, retention preview, storage
health), **H06** (preserve matched-rule ID/class and revision at event time),
serving **R05** (explain "why allowed/blocked?"). Source rows:
[competitive feature roadmap](2026-10-24-competitive-feature-roadmap.md)
lines for H01, H02, H06, R05; plan item 8 of
[the overnight run](../plans/2026-10-27-overnight-autonomous-run.md).
Later items that depend on this (H03 filters, H05 baselines, H08 digests, Q07
budgets) are out of scope; Q07's broader tuning stays deferred, but the read
limits in [D8](#d8-reading-history) are part of this design.

## Decisions requested

| # | Decision | Recommendation |
|---|----------|----------------|
| D1 | Who writes history | The **host bridge**, not the GUI. It records what the bridge *receives*, which is not everything the daemon decides (see D1 coverage) |
| D2 | Default | **Off.** Enabled only by an explicit user action, consent stored in the bridge |
| D3 | Store | One SQLite file in the bridge state directory; strict 0700 directory, 0600 files, via `sqlite_file::open_owner_only` |
| D4 | What is stored | A fixed field allowlist. Command line, `process_args`, `process_env` and rule conditions on them are never stored. **Raw executable and destination strings are stored and may contain user-chosen text; this is a stated limit, not a guarantee** |
| D5 | Retention and disk | Age (default 30 days) and a 64 MiB total budget split into main-file, journal and slack shares; invariants S1-S5 (field caps, page-bounded transactions, no full VACUUM); oldest first; admission quotas against floods |
| D6 | Gaps | Session ledger; invariant: coverage never passes an uncommitted item; every discontinuity is an explicit gap; permanently unobservable traffic is labelled as such |
| D7 | Rule identity | Rule name + semantic hash + a redacted display snapshot (hash and snapshot are separate); captured by a per-prompt outcome guard for interactive decisions; `sensitive` is hashed |
| D8 | Reads | Paged, byte-bounded, bridge-wide admission, read-only opener, off the verdict path |
| D9 | Encryption at rest | Not in v1; rely on file permissions and say so |
| D10 | Multi-user (system bridge) | **Not supported in v1**; history is disabled there |

## What exists today (grounding)

- **Events.** opensnitchd dials the bridge as a gRPC client. The translator
  turns each connection event into a `ConnectionRow`
  (`translator/connection.rs`, `ws_messages/verdict_wire.rs`) with `id,
  process, process_path, dst_host, dst_ip, dst_port, protocol, direction,
  action, bytes_sent, bytes_received, started_at_ms, matched_rule,
  auto_answer, decided_by_default`. The daemon event also carries
  `process_args` and `process_env`; the translator does not copy them (the
  row-field list is pinned by a test in `analysis_export/snapshot_tests.rs`).
- **Memory only.** `ConnectionCache` keeps the most recent rows (default
  10,000), evicting the oldest non-pending rows, with no coverage ledger.
- **Row caveats history inherits.** Byte counters are placeholders and
  direction is hardcoded outgoing (see the
  [export design](2026-10-24-ai-ready-cache-export-design.md)). Prompt rows
  are stamped with bridge construction time; decided rows use daemon event
  time.
- **Matched rule today.** `matched_rule` is the rule *name* only; a
  default-action event has none and sets `decided_by_default`. The inspector
  resolves the name against the *current* rule list, so an edited or deleted
  rule rewrites the explanation. That is the R05 gap H06 closes. The existing
  matched-rule display and "Show rule" navigation are preserved and are not
  re-sold as new.
- **What reaches the bridge, and what does not** (this bounds every coverage
  claim below):
  - Periodic events come from opensnitchd pings (`Statistics.events`), which
    are incomplete by construction: capped per ping (oldest dropped), emptied
    before the daemon knows the ping arrived, never resent, and nothing is
    reported while no bridge is connected. A `nolog` rule produces **no event
    at all** (`cache/rule_hits.rs` module doc).
  - Stock opensnitchd v1.8.0 appends **no event** for a connection that matched
    no rule; the bazzite-tower fork appends one with a marked synthetic rule
    (`daemon_contract::is_default_action_rule`). On stock, default-applied
    traffic is unobservable.
  - Interactive prompts (`AskRule`) are rejected before any row exists when no
    GUI is authenticated (`grpc_server.rs`); the daemon then applies its
    default action and the bridge sees no prompt row.
  - The existing loss accounting `missing = Δrule_hits − received`
    (`cache/rule_hits.rs`) detects lost *rule* events within a daemon run and
    notes `last_gap_unix_ms`; it deliberately does not count default-action
    events (`rule_misses`).
- **Local persistence precedents (stated accurately).**
  - Learning mode (`kirigami/src/learned*`, `paths.rs::learned_path`): GUI-side
    under `$XDG_CONFIG_HOME`, 0600, advisory lock file. `SEEN_CAP` bounds
    in-memory de-duplication ids only; the persisted aggregate is bounded by
    per-class caps and a 16 MiB budget, and expiry discards the **whole**
    aggregate 30 days after its creation. It is therefore a precedent for
    permissions and bounded files, **not** for rolling per-event retention,
    which this design adds.
  - Bridge state (`bridge-cli/src/storage.rs`): the state directory is
    canonicalised once; the per-user check only removes group/other *write*
    (it permits 0755); SQLite holds blocklists and profiles; a storage problem
    never stops the bridge, it degrades and is shown to the GUI. History does
    **not** reuse that check as sufficient (see D3).
  - Export design: persistent history was deferred to this document;
    multi-user/UID authorization was left open.
- **Access boundary today.** Any authenticated WS client reads all live rows;
  the boundary is the Unix socket (0700 directory, 0600 socket) **plus the
  handshake token**. History adds no authentication, so **whoever can open the
  socket and present the token can read history through the bridge, with no
  access to the database file needed.** The design must not widen that.

## D1. Where history is written, and what that can cover

**Recommendation: the host bridge writes; the GUI reads over the existing
WS.** The bridge sees events while the GUI is closed, already owns durable
state and the degrade-to-memory pattern, and holds the daemon's `Rule`.
Cost: a new WS request/response family and a capability flag so an older
bridge shows "history unavailable", never an empty list.

**Coverage is what the bridge receives, not what the system did.** History is
labelled "events the bridge received" and the doc does not claim continuous
bridge uptime means complete coverage. Required daemon behaviour and the
resulting standing labels:

| Traffic | Recorded? | Requires | Label when not |
|---------|-----------|----------|----------------|
| Connections matched by a logging rule | Yes, as a lower bound | Ping events (stock or fork) | Loss counted via `Δrule_hits − received` (D6) |
| Connections matched by a `nolog` rule | **No** | Nothing can provide it | "Not observable: `nolog` rules" |
| Connections matching no rule, default action applied | Only on the fork | Fork default-applied events | On stock: "Not observable: default-applied traffic" |
| Prompts while a GUI is authenticated | Yes (D7 capture) | GUI session | n/a |
| Prompts while no GUI is authenticated | **No row exists** (rejected before a row is made) | Nothing | "Not observable: no GUI session" |
| Anything while the bridge is down or the daemon disconnected | No | n/a | Gap (D6) |

The capabilities of the connected daemon (stock vs fork) are part of the
history status, so the UI states which of the rows above applies today.

## D2. Opt-in

- Off by default, on every install including upgrades. No migration turns it
  on. Nothing is created on disk until the user enables it.
- Enabling is a dedicated Settings action that states, before confirmation:
  the stored fields (D4) **and that raw program paths and destination names
  are stored**; the bridge's actual file location (reported by the bridge, D3);
  who can read it (threat model); the default retention; what is not
  observable (D1 table); how to turn it off and delete it.
- The enabled flag and retention settings live in bridge state, so the bridge
  never records on a stale GUI preference.
- Disabling stops recording at once, via the barrier in H02. The dialog offers
  "Disable and delete history" (default) and "Disable and keep".
- If storage is unusable the bridge keeps running, history reports
  `unavailable: <reason>`, and it never retries into another location.

## D3. Storage location, format, permissions

- **Format:** one SQLite database. Tables: `events`, `rules_seen` (D7), `gaps`
  and `sessions` (D6), `meta` (schema version, consent record).
  `secure_delete=ON`. Chosen over JSONL because retention, counts for
  preview, and range delete need indexes and transactions.
- **Path:** the bridge's state directory, `<state>/history/history.db`. The
  Flatpak ships only the GUI; **the bridge runs on the host** as a systemd
  user service (`packaging/flatpak/...yml` header; unit `StateDirectory=
  snitchwatch`), so the path is the host path, normally
  `~/.local/state/snitchwatch/history/`, **not** under `~/.var/app/...`.
  The GUI cannot compute it; the bridge reports the actual path in the history
  status and the opt-in and backup text use that reported value. A
  sandbox-resident writer would be a separate deployment decision and is not
  proposed.
- **Permissions and validation, explicit (not inherited):**
  - The history directory is created 0700 and **must be exactly 0700 owned by
    the bridge's uid**, checked on every open with `O_NOFOLLOW|O_DIRECTORY`;
    anything else makes history `unavailable`. The existing per-user state
    check is not sufficient because it tolerates 0755.
  - The writer opens the database only through `sqlite_file::open_owner_only`
    (existing in `snitchwatch-bridge/src/sqlite_file.rs`): `O_NOFOLLOW`,
    0600, regular-file/owner/link-count checks, refusal of unsafe
    `-journal/-wal/-shm` sidecars, `trusted_schema=OFF`, defensive mode. The
    schema is checked with `require_known_schema`. A newer schema is refused
    (`unavailable: newer schema`).
  - Created under a restrictive umask so no wider mode exists transiently.
- **State, not config:** regenerable log data stays out of `~/.config`.

## D4. What is stored

Exactly this allowlist per event (an insert API that takes only these fields):

| Field | Notes |
|-------|-------|
| `ts_ms` | Event time; for prompts also `decided_ts_ms` (D7). Construction-time stamps are not used |
| `process_path` | Executable path as reported; NULL when absent |
| `dst_host`, `dst_ip`, `dst_port`, `protocol` | Destination as seen |
| `action` | allow / deny / blocklist / default-allow / default-deny / pending-unanswered |
| `answered_by` | Classification table below |
| `rule_name`, `rule_hash`, `rule_class` | D7. `rule_class` derived from rule-name prefixes already in use (`snitchwatch-default-*`, `z00-blocklist:*`, else `user`); NULL name/hash for default events |
| `repeat_count` | Default 1; exact count of identical coalesced events (D5) |
| `uid` | Only if open question 1 is approved; otherwise not stored |

**Never stored, enforced by test (below):** `process_args`, the command line,
`process_env`, payloads, URL path/query, byte counters and direction (still
placeholders), the ephemeral row `id`, and **any rule condition on
`process.command` or `process.env.*`** (D7).

**Stated limit on raw strings.** `process_path`, `dst_host` and rule operands
over paths/hosts are user- or attacker-chosen text and can embed secrets
(a token in a file name, a credential in a hostname). Heuristic detection
cannot make this absolute, so the design does **not** promise "no secrets";
it promises no *fields* that routinely carry them. Reducing the exposure
further is an owner decision (open question 9: store basename plus path hash,
or a user exclusion list). Strings are length-capped (path 4096, host 255)
and shown as plain text.

`answered_by` classification (exhaustive; unknown provenance is stored as
unknown, not guessed):

| Source | Value |
|--------|-------|
| Daemon event with a matching named rule | `rule` |
| Daemon event with the marked default-action rule / `decided_by_default` | `default` |
| `AutoAnswer::FilterPaused` | `filter-paused` |
| `AutoAnswer::NoAnswer` (daemon applied its default after the prompt timeout) | `no-answer` |
| Verdict arriving from an authenticated GUI via `SetVerdict` | `gui` — **human or silent mode is not distinguishable today**: silent mode sends an ordinary `SetVerdict` with no provenance (`silent_mode.rs`, `ws_messages.rs`) |
| `AutoAnswer::Unknown` or any other | `unknown` |

Adding explicit provenance to `SetVerdict` (so `silent-mode` can be recorded)
is a protocol change and is open question 6; until approved it is `gui`.

## D5. Retention, disk budget, admission

Two independent limits; whichever trips first evicts.

- **Age:** default 30 days; choices 7 / 30 / 90. No "forever".
- **Disk: invariants.** `B` is a single total budget for everything the
  feature may put on disk (default 64 MiB; choices 16 / 64 / 256 MiB; ceiling
  256 MiB). Each invariant has one enforcement mechanism and one test; where a
  number is not yet known, the implementation must prove it (see open
  question 2) and this design does not guess it.

| # | Invariant | Enforcement mechanism | Test |
|---|-----------|-----------------------|------|
| S1 | Every persisted field has a fixed maximum byte length, so the largest possible row, index entry and snapshot are known from the schema | One field-cap table in the insert API (`rule_name`, `process_path`, `dst_host`, rule snapshot parts and every other column); longer values are truncated with a marker or the row is refused; `page_size` is pinned (4096) at creation and verified on open | Property test: for every column, a value at cap+1 never reaches the file at more than cap |
| S2 | No write transaction can modify more pages than the journal share can hold | The writer sizes each transaction from S1's worst-case pages **per row including indexes, snapshot rows and orphan-snapshot cleanup**, and commits when the page budget is reached. **The implementation must prove the per-row page constant from the schema and caps; it is not asserted here.** If it cannot be proved, fall back to reserving headroom for a full rollback journal (main-file share ≤ B/2) | Fault test: worst-case rows (all fields at cap) through insert, eviction, snapshot cleanup and incremental vacuum; assert pages modified per transaction never exceeds the budget |
| S3 | main file + maximum journal + slack ≤ B | `max_page_count` for the main file; journal = S2's page budget × page size; 10% slack never allocated; 2% reserved for the ledger (D6) | Measured simultaneous file sizes under S2's worst-case workload stay ≤ B (measurement validates the invariant; it is not the enforcement) |
| S4 | Freed space returns to the filesystem without a full rewrite | `auto_vacuum=INCREMENTAL`, `incremental_vacuum(N)` in steps that obey S2. **No full `VACUUM`**: its temporary space cannot fit in B | Delete 90% of rows; file shrinks; no step exceeds S2 |
| S5 | Deleted content is not left readable in the file | `secure_delete=ON`; clear-all recreates the file (H02) instead of deleting inside it | Canary scan after delete and after clear |

  `journal_size_limit` only trims the journal after a commit and is not
  relied on for any of the above.
- **Eviction order:** per-process byte share first (admission, below), then
  oldest `ts_ms` first, in chunks obeying S2. If a chunk
  does not bring usage under the high-water mark after a fixed number of
  rounds (default 3), recording pauses with a `write-failed (cap)` gap and
  retries with exponential backoff; it never grows past the budget and never
  deletes unrelated files. Gaps, sessions and snapshots follow D6/D7.
- **Insert path never blocks decisions.** Events enter a queue bounded by
  **bytes (default 2 MiB) and count (4,096)**, whichever is hit first, and
  are written in batches by one writer thread. A full queue drops the event
  and counts it into a gap. A slow or failing disk must never delay a verdict
  reply to opensnitchd.
- **Admission and quotas (the real flood protection).** A runaway or hostile
  process must not be able to evict other applications' history.
  - Coalescing identical consecutive events within 1 s into `repeat_count` is
    **duplicate compression only** and is not claimed as a rate cap.
  - A global admission budget (default 200 events/s sustained, burst 1,000)
    and a per-`process_path` token bucket (default 20 events/s, burst 100).
  - **A per-process share of retained *bytes*, not rows (default 25%).**
    Large records must not let one process fill the budget while staying under
    a row share. The share counts the process's events **and the rule
    snapshots attributable only to it**. It is enforced **before** global
    eviction: once a process is over its byte share, its own oldest rows (and
    snapshots that become unreferenced) are evicted first, and only then is any
    other process's data touched. Test: a mixed-size flood of large records
    from one process cannot cause eviction of another process's small records
    while the flooder is over its share.
  - Every refusal increments an in-memory `admission-drop` counter, and drops
    are never silent, but the bookkeeping is itself bounded: the token-bucket
    map holds at most **1,024 entries** and expires entries idle for 60 s
    (a process-path flood therefore evicts idle buckets, never grows memory);
    the offender table keeps the top 64 by drops; counters are flushed as
    **one aggregated gap row per cause per minute**, never one row per
    refusal; the ledger tables are capped (D6) and use the 2% reserved
    ledger space, so refusals cannot fill the database or stop unrelated
    recording.
  - The numbers are initial proposals needing owner confirmation (open
    question 2); the mechanism is part of the design.

## D6. Gap and loss labelling

History is only honest if its holes are visible.

**Session ledger.** `sessions(start_ms, end_ms, end_kind)` plus
`meta.watermark_ms`, the durable coverage watermark.

**Invariant W1: coverage never advances past any accepted-but-uncommitted
item.** An item is *accepted* when the bridge takes responsibility for it and
*committed* when its durable event or gap row is written. The accepted
uncommitted items are: queued daemon events, in-flight batches, pending
prompts (guards), and **in-memory loss and drop counters** (admission drops,
queue overflow, upstream-loss tallies). Enforcement: every accepted item takes
a **pin** recording its arrival time in a single pin registry; the watermark
written by any batch or heartbeat commit is `min(commit time, oldest pin)`,
computed in the same transaction as the commit; a pin is released **atomically
with** the commit of that item's durable event or gap row (same transaction),
never earlier. A counter is flushed to its aggregated gap row and its pin
released in one transaction. Test: kill -9 with (a) a queued daemon event,
(b) a pending prompt, (c) an unflushed drop counter, each followed by a
heartbeat commit; the reconstructed gap begins no later than the pinned
item's arrival in every case.

**Invariant L1: the ledger is bounded in bytes and rows, with a defined
overflow.** `gaps` and `sessions` are capped by a byte limit (the 2% reserve,
S3) and a row count. Adjacent same-cause gaps merge on write; entries older
than the oldest retained event compact into one `evicted` marker. When either
ceiling is still exceeded (for example many recent, non-mergeable restarts),
the **oldest recent entries are compacted into a single explicitly
`uncertain` coverage span**: "coverage between X and Y is uncertain; N
restarts/gaps occurred", which never claims continuity and loses only detail.
Test: thousands of non-mergeable restarts keep the ledger within both limits
and produce one uncertain span, never a silent loss of a gap.

- Every time recording starts, the bridge compares against the previous
  session: a **clean stop** has an exact `end_ms`, so the gap from that
  `end_ms` to the new `start_ms` is recorded as `bridge-down` with exact
  bounds; a **crash or kill** has no clean end, so the gap runs from
  `watermark_ms` (uncertain start, shown as "between X and Y, exact start
  unknown") to the new start. A clean shutdown therefore **does** produce a
  gap; the earlier "clean shutdown newer than last event" rule is dropped.
- If history was disabled in between, the interval is `disabled`.

**Gap causes:**

| Cause | When recorded |
|-------|---------------|
| `bridge-down` | Session discontinuity above |
| `daemon-disconnected` | opensnitchd stream closed (the bridge tracks `daemon_down` and a generation counter) until the next HELLO. Events the daemon discards while no bridge is connected are lost by design |
| `upstream-loss` | Reuses the existing `cache/rule_hits.rs` accounting (`Δrule_hits − received`, counter resets, failed pings, capped batches): `missing` events become a gap with a **lower-bound** count and the uncertainty flags the module already tracks. Not derived from WS `Lagged`, which measures downstream subscriber loss, not daemon loss |
| `queue-overflow` | History queue full (D5) |
| `admission-drop` | Quota refusal (D5), with per-process counts |
| `write-failed` | Storage error, cap pause, or disk full |
| `disabled` | Recording was off |
| `evicted` | Retention removed events; shows the earliest remaining time and removed count |
| `clock-jump` | Wall clock moved backwards or beyond a threshold; ordering in that interval is unreliable |

**Permanent unobservability** (D1 table: `nolog`, stock default-applied
traffic, no-GUI prompts) is not a gap with bounds; it is a standing note on
the history view. A summary over a range states covered fraction **and** these
standing exclusions. The view never shows an empty interval without a gap or
an explicit "no events recorded while history was on" statement, which is
valid only when the session ledger shows continuous recording and the
exclusions do not apply to the user's setup.

GUI closed is not a gap (the bridge records regardless), except for the
no-GUI-prompt row above, which cannot be recorded at all.

## D7. Event-time rule identity (H06) for R05

Goal: answer "why was this allowed or blocked?" as of the event, even if the
rule has since been edited or deleted.

### Capture paths

- **Daemon events** carry a full `Rule` (matched-rule pings). The bridge
  records identity from that message at ingestion, not from a later lookup.
  The module doc of `rule_hits.rs` still applies: these are lower bounds.
- **Interactive decisions** have no rule at event time: `AskRule` carries only
  a `Connection`, and the reply rule is built later (`grpc_server/replies.rs`).
  Rules the bridge sends back are also not re-reported as events (once-rule
  events generated by the bridge are deliberately suppressed in
  `translator/connection.rs`). Not every outcome constructs a reply rule: a
  `NoAnswer` timeout returns `Err(Status)` from the answer wait
  (`grpc_server/answer_wait.rs`, `grpc_server.rs`) and skips reply
  construction entirely. So there is no single construction point to hook.
  The design instead requires a **per-prompt outcome guard** created when the
  `AskRule` handler accepts a prompt, which records exactly one terminal
  outcome on every exit path (including drop/cancellation):
  `replied(final rule)` (including pause replies), `timeout`
  (`NoAnswer`, no rule), `error` (terminal `Status`), `gui-lost` (GUI
  disconnected while pending) or `cancelled` (daemon cancelled the call).
  Definition:
  - one history event per prompt, keyed by the prompt's connection id;
  - `ts_ms` = prompt arrival (bridge clock), `decided_ts_ms` = terminal
    outcome time;
  - **Invariant G1: eligibility and generation are fixed at acceptance.**
    When the guard is created it captures (a) whether recording is enabled and
    the consent record in force, and (b) the current generation. Both are kept
    through settlement. At settlement the outcome is accepted only if the
    captured generation equals the current one **and** recording is still
    enabled under the same consent; otherwise the outcome is discarded, its
    snapshot is never written, and its watermark pin is released under the
    control gate (so a clear, or a disable then re-enable, can never be
    repopulated by a prompt that was already waiting). A guard is also a
    pending-item pin for W1 until it settles or is discarded. Test: clear, and
    disable then re-enable, while prompts are still pending; after the
    prompts settle, no row, snapshot or pin from before the control operation
    exists.
  - **settled** when the guard records an outcome; only then is the event
    queued (subject to G1).
  - **duplicates, never merged by guess.** A later ping event can describe the
    same connection (for example a persistent rule created by the reply), but
    the available fields (program, destination, port, protocol, rule name)
    do not identify a connection: rule names are deterministic and the source
    port is not carried, so two distinct connections can share all of them.
    The bridge therefore **merges only on verified correlation** (a shared
    connection id, or a source port if open question 11 approves storing it).
    Otherwise both events are kept and marked `possible-duplicate`, and any
    count that could include both says so ("may include decisions counted
    twice"). Nothing is silently dropped or merged.

### Identity fields

- `rule_name` (as today).
- `rule_hash`: SHA-256 truncated to 128 bits over a canonical encoding of the
  **redacted** rule (below): name, action, duration, enabled, precedence,
  `nolog`, and the operator tree (type, operand kind, `sensitive`, redacted
  data) in fixed order, no timestamps. It is computed
  from the redacted form so no digest of a withheld value is ever stored: an
  unsalted digest of a low-entropy value (a command-line fragment, an
  environment value) would let anyone with the file confirm guesses.
- `rule_class` as in D4.
- No hash for default-action events; `answered_by = default` explains them.

### Redacted display snapshot

`rules_seen(rule_hash, snapshot, first_seen_ms)` stores a display copy built
by a **recursive allowlist**, never by copying the rule:

- Keep structure: action, duration, enabled, precedence, `nolog`, operator
  type and nesting, and **`Operator.sensitive` on every operator, including
  nested ones**: it changes case-sensitive matching, so it is shown in the
  snapshot and is part of the hash. A sensitivity-only edit must change the
  hash (test below).
- Keep operand *values* only for these operand kinds: `process.path`,
  `dest.host`, `dest.ip`, `dest.port`, `protocol`, `user.id`, and `lists.*`
  (the list directory path, see below). Everything else, **including
  `process.command`, `process.env.*` and any operand kind this build does not
  know**, is withheld: kind kept, value replaced by `[withheld]`. Free text
  fields (description) are dropped. Nested `list` operators are processed
  recursively with the same rule; depth and size are capped.
- Consequence for fidelity: a rule that depends on withheld conditions is
  shown as "contains N conditions that are not recorded", and **change
  detection for such a rule covers only its visible part** (the hash is over
  the redacted form). The UI says so instead of claiming "unchanged".
- Operand values that are paths/hosts keep the stated limit in D4: they are
  the user's own rule text and may embed secrets; the snapshot is covered by
  the same clear-all and retention.

### List-backed rules

For `lists.*` operators the rule names a **directory**; membership can change
without the rule changing (the blocklist materializer replaces list contents
at the same location). The rule hash therefore does not capture what matched.
The UI qualifier is mandatory: "rule definition unchanged; the contents of
the referenced list are not recorded". Recording a list revision requires the
blocklist subsystem to expose an applied-content revision (open question 7);
until then history makes no claim about list contents.

### Bounds

At most 2,000 snapshots, 8 KiB per snapshot, 4 MiB total. Snapshot eviction is
**independent of events**: when over any bound, the oldest snapshot is dropped
even if events still reference it (events keep `rule_hash`; the UI says "rule
definition no longer retained"). So the limit always has an eviction victim.

**Snapshot cleanup on every deletion path.** Snapshots hold rule paths and
hosts, so they must not outlive their last event. Expiry, range delete, limit
reduction and capacity eviction each delete, **in the same transaction**, every
snapshot with no remaining referencing event (`NOT EXISTS`). Clear-all removes
the file. A test deletes a single event below the cap and asserts its
now-unreferenced snapshot bytes are gone.

### UI behaviour (R05)

The inspector shows the recorded name and, if the current rule has the same
hash, "definition unchanged" (qualified as above for list-backed and
partially-withheld rules); a different hash shows "this rule has changed
since" with the recorded snapshot beside the current rule; a missing rule
shows "this rule has been deleted" plus the snapshot. Editing or deleting a
rule never rewrites stored events.

## D8. Reading history

- Paged only: a request returns at most 200 rows **and** 256 KiB per response,
  with a keyset cursor (`(ts_ms, id)`), never `OFFSET`. There is no
  "return everything".
- **Bridge-wide bound, not just per connection.** A global, non-blocking
  admission gate allows at most 2 history queries at once across all WS
  connections, with 2 bounded response slots of 256 KiB; a request that cannot
  get a permit immediately is refused with `busy` and is never queued. Permits
  are owned by the blocking worker and released only when that worker has
  actually exited, so a cancelled query still holds its slot until it stops.
  Readers are also part of the clear/disable gate (C1). This follows the pattern already used by the cache export
  (`bridge-cli/src/analysis_export.rs`, admission and response slots). Per
  connection, a new query or a disconnect cancels the previous one. A query has a 2 s time budget and
  returns partial-with-cursor on expiry.
- Reads run on a **separate read-only SQLite connection** on a dedicated
  blocking pool. That connection needs a **validated read-only opener**:
  `sqlite_file::open_owner_only` always opens writable and may create/chmod
  the file, so a new variant (`open_owner_only_readonly`) must apply the same
  checks (`O_NOFOLLOW`, regular file, owner, link count, unsafe-sidecar
  refusal, `trusted_schema=OFF`, defensive mode) while opening read-only
  without creating or changing modes, and set `query_only=ON`. They never run on the verdict path and never hold the cache or
  verdict lock during SQL, hashing or serialization. If reads and the writer
  contend, reads yield; verdict latency is never traded for history.
- The GUI keeps a bounded window of pages (the existing row store appends
  without a capacity limit, so history rows must not be appended to it
  unbounded); older pages are discarded and re-fetched by cursor.
- Anyone who can open the socket and present the token can issue these reads.

## H02. Clear, preview, barrier, health

**Invariant C1: after a control operation is acknowledged, nothing inside
its deletion scope is readable or writable, and nothing outside its scope is
lost without a durable gap.** Writers *and* readers are covered by one control
gate. The **scope** of a control operation is the set it is approved to
delete: everything for clear and disable-and-delete; the previewed set for a
range delete; the rows the new limit would evict for a retention change;
nothing for disable-and-keep. Enforcement, in this order:

1. **Validate before any destructive step.** Under the control gate, but
   before touching the queue, readers or files: check the preview token and
   its checksum, the C2 conditions, and compute the exact scope. If
   validation fails, the operation is rejected and **nothing has changed**:
   queued events, pending guards and pins are untouched and ingestion was
   never closed.
2. Increment the generation under the queue gate. Events accepted from now on
   carry the new generation. Guards created earlier are handled by G1.
3. **Close read admission** (D8 gate refuses new queries with `busy`),
   **cancel and join every reader worker, and close every SQLite handle**,
   including the read-only connection.
4. **Split the queue by scope; do not discard wholesale.** Accepted queued
   events and pending guards **outside the scope are kept** and re-stamped
   to the new generation (a retention change keeps what the new retention
   keeps; a rejected or narrower range delete keeps everything it does not
   cover). Items **inside the scope** are discarded together with their
   pending snapshots. **Any accepted item discarded outside an approved scope
   (for example because it cannot be kept) first gets a durable gap row, and
   only then is its pin released** (W1): a pin is never released by a
   discard alone.
5. Perform the operation (below) and acknowledge after cleanup.
6. **Invalidate** any already-produced response and GUI page of an older
   generation: responses carry the generation, the bridge refuses to deliver
   a stale one, and the GUI drops pages whose generation differs.

Tests (each with a blocked writer and a non-empty queue):
- a **retention change** keeps the queued events the new retention keeps,
  discards only those it evicts, and leaves the watermark correct;
- a **rejected stale preview** leaves the queue, guards, pins and watermark
  exactly as before, and ingestion was never interrupted;
- an accepted item that must be dropped outside scope leaves a durable gap
  before its pin is released (kill -9 right after shows the gap);
- a reader mid-query when clear arrives (its handle is closed before unlink,
  no deleted row is delivered afterwards); a stale page after clear is
  dropped.

- **Clear all** unlinks the database and sidecars and recreates it empty (no
  giant delete transaction, no journal growth) **only after** steps 1-4 above,
  so no reader holds the old file. **Invariant C2: clear-all deletes only the
  previewed set or fewer.** Under the same writer barrier as the deletion, the
  bridge checks that no row exists above the previewed maximum row id and that
  the previewed checksum still matches; if either fails nothing is deleted and
  a fresh preview is required. (The alternative, falling back to bounded
  deletion that preserves newer rows, is allowed instead of refusal; open
  question 12.) The UI reports counts and bytes only after confirmation; on
  failure it says exactly what remains. Range delete and limit reduction use
  bounded chunks obeying S2, plus snapshot cleanup.
- **Retention preview** (for clear, a range delete, or lowering a limit):
  the bridge returns `N events from A to B, about X MB` plus an **expiring
  preview token** (60 s) bound to the generation, the maximum row id, and a
  checksum of the previewed set (row count, sum of row ids and of
  `repeat_count`). A cutoff row id alone does not fix the set, because
  retention can remove rows and coalescing can raise a `repeat_count` before
  the user confirms. On confirm the bridge recomputes the checksum; if it
  differs, or the token expired, nothing is deleted and the user is shown a
  fresh preview. Counts are exact at preview time; bytes are an estimate and
  labelled so.
- **Storage health:** enabled, reported path, usage against each share of the
  budget, event count, oldest event, current session, queue drops, admission
  drops, current gap, last error, daemon capability (D1). Failures raise a
  visible banner, not just a log line.
- **Deletion guarantees, stated in the UI:** removal from the database file
  and its free pages. Not from backups, filesystem snapshots, disk journals,
  SSD remnants, or exports the user saved.
- History is excluded from the AI-ready cache export in v1.

## Threat model

**Assets.** A time-ordered record of which program contacted which
destination. It reveals browsing and usage habits, VPN/Tor use, software
inventory, work patterns, and sensitive sites. It is a profile of the user.

**Who can read it.**

| Reader | Can read? | Notes |
|--------|-----------|-------|
| Same-uid processes | **Yes** | 0600 stops other users, not other processes of the user. A same-uid process can also use the socket plus token to read history through the bridge |
| Any holder of the socket path **and** token | **Yes** | History is exposed over the WS exactly as live rows are |
| Other local users | No | 0700/0600; the system-bridge case is why D10 |
| root | Yes | Out of scope |
| Backups of `~` or the host state directory | **Yes** | Long-lived, often off-box. The opt-in text names the bridge-reported path so the user can exclude it |
| Other Flatpak apps | **Only if they have been granted host access** | The database is a host file owned by the user. Snitchwatch adds no permission for it, but another Flatpak granted `--filesystem=home`/`host` (or the state directory) can read it as the same uid, and any app that can reach the bridge socket and token can read history through the bridge. Snitchwatch cannot control other apps' grants; the opt-in text says so |
| Disk theft / offline access | Yes | See encryption |
| Remote | No | No listener, no upload path |

**Sensitive-data minimisation.** The D4 allowlist and the recursive snapshot
allowlist (D7) are the controls. Residual, stated exposure: raw
`process_path`, `dst_host` and non-withheld rule operand values, which can
embed secrets. Mitigations on the table, not in v1 by default: basename+hash
paths, per-app and per-host exclusions (open questions 3 and 9). Hashing
hostnames at rest is rejected: it removes the feature's purpose.

**At-rest encryption.** (a) none, rely on permissions and disk encryption
(recommended v1, matches existing files); (b) SQLCipher with a keyring key:
protects against casual disk/backup exposure only (a same-uid process can ask
the keyring too), needs a keyring that may be locked or absent, adds a C
dependency to a reproducible build, risks data loss on keyring reset; (c)
passphrase: needs a prompt the background service cannot show. The UI must
not say "encrypted" or "secure".

**Integrity.** A same-uid attacker can edit history. Not defended; hashes give
revision identity, not authenticity; the UI does not call it an audit log.

**Injection and untrusted text.** Bound SQL parameters only; no dynamic SQL;
plain-text display.

**Resource abuse.** D5 budgets, quotas and bounded queue; D8 read limits.

**Failure behaviour.** Closed: a storage failure means not recording and
saying so; never a fallback path.

**Deletion.** H02 barrier makes clear/disable atomic with respect to queued
writes; copies outside the file are not reachable.

## Alternatives considered

1. **GUI-side history.** No bridge protocol, but loses all events while the
   GUI is closed; coverage claims would be false for most of the day.
2. **Rely on opensnitchd's own loggers.** Outside Snitchwatch's retention and
   clear controls, cannot carry rule snapshots or label bridge-side gaps.
3. **JSONL with rotation.** Easy, but age/size retention, preview counts and
   range delete become rewrites; torn lines on crash. Rejected for SQLite.
4. **Hash hostnames at rest.** Destroys the main use. Rejected as default.
5. **SQLCipher / per-record keys.** Deferred (see threat model).
6. **Rule revision log (every edit).** Needs hooks in every rule writer;
   event-time hash + redacted snapshot answers R05 without it.
7. **Hash the full unredacted rule.** Gives exact change detection but stores
   a guessable digest of withheld command/env conditions. Rejected.
8. **Tamper-evident chain; keep forever.** Rejected (overkill; contradicts
   bounded growth).

## Verification plan (for the later implementation, not done here)

- **Canary tests on database bytes.** A canary in `process_args`,
  `process_env`, a `process.command` rule condition, a `process.env.*` rule
  condition, nested list operators, an unknown operand kind and a description
  must not appear anywhere in the DB, sidecars or snapshots. Because a
  plaintext scan cannot detect a hash taken over an unredacted secret, a
  **redaction-equivalence test** varies *only* withheld values (command and
  environment conditions, unknown operand kinds, descriptions, including inside
  nested lists and duplicated list JSON) and asserts the redacted canonical
  bytes **and the resulting hashes** are identical. A second test changes only
  `Operator.sensitive` and asserts the hash changes. Separate tests
  assert that a canary placed in a *path* or *host* **does** appear (documenting
  the stated limit) so the limit cannot silently change either way.
- **Allowlist test** that fails if a new field reaches the insert API.
- **Invariant tests** S1-S5, W1, L1, G1, C1 and C2 as tabled above, each
  adversarial (worst-case rows, kill -9, stale prompts, mid-query clear,
  newer-row-after-preview), not only happy path.
- **Budget tests** are the S1-S5 tests above; they measure main file and
  journal together as a validation of the invariants, not as their
  enforcement.
- **Flood tests:** alternating-destination flood from one process cannot evict
  another application's history beyond its quota; drops are counted.
- **Barrier tests:** blocked writer with a non-empty queue; clear/disable
  leaves zero rows afterwards; a preview token is refused after retention or
  coalescing changes the set, and when expired.
- **Gap tests:** clean stop, kill -9, daemon disconnect, full queue, full
  disk, counter reset (`rule_hits` module cases), `nolog`/stock-default
  labels. Watermark never covers uncommitted events.
- **Capture tests with `mock_opensnitchd`** (never a live daemon): interactive
  allow, deny, fallback and `FilterPaused`/`NoAnswer` replies; two distinct
  connections sharing program, endpoint and rule are both kept and flagged;
  timeout, error, GUI-lost and cancel outcomes each record exactly one event;
  a pending prompt pins the watermark across a kill -9; rule edited/deleted afterwards leaves the stored explanation intact.
- **Permission tests:** directory exactly 0700, files 0600, symlink and
  wrong-owner refusal, unsafe sidecar refusal.
- **Read-limit tests:** page and byte caps, cancellation, no verdict-lock
  contention, bounded GUI window.
- A GUI test for each gap label, each standing "not observable" note, and
  "unavailable" on an older bridge.

## Open questions for the owner

1. **Multi-user.** The system bridge sees all users' events; v1 disables
   history there (D10). Per-uid rows with uid-scoped reads (needs an
   authorization design), or per-user-bridge only?
2. **Numbers.** Confirm defaults: 30 days; 64 MiB total budget (ceiling
   256 MiB); admission 200 events/s global, 20/s per process and a 25% byte
   share per process. The per-row page constant behind S2 and the field caps
   behind S1 are not guessed here: the implementation must derive and prove
   them from the schema, and if S2 cannot be proved the fallback is
   reserving a full-database journal (main file ≤ half of B). Which cap
   values to use for `rule_name`, `process_path` and `dst_host` is for the
   owner to confirm.
3. **Exclusions.** Per-application and per-host "don't record" lists in v1
   (recommended, small) or defer?
4. **Eviction bias.** Oldest-first regardless of verdict (proposed), or keep
   denies longer than allows?
5. **Daemon-side loss.** The `upstream-loss` gap is a lower bound from the
   existing counters. Is that enough, or should daemon changes be pursued for
   exact counts, `nolog` visibility, or no-GUI prompt capture?
6. **Provenance.** Add explicit provenance to `SetVerdict` so silent-mode
   answers can be recorded as such (protocol change), or keep `gui` meaning
   "human or silent mode"?
7. **List-backed rules.** Have the blocklist subsystem expose an
   applied-content revision so history can say what a list contained, or
   accept the "list contents not recorded" qualifier permanently?
8. **Encryption.** Accept "none, rely on permissions" for v1, or require a
   keyring-backed option first?
9. **Raw path/host retention.** Store full process paths and destinations
   (proposed, with the stated limit), or basename + path hash, or require the
   exclusion lists (question 3) before shipping?
10. **Backups and disable default.** Should the opt-in text give a specific
    backup-exclude recipe for the reported path, and is "Disable and delete"
    the right default button?
11. **Source port.** Store the connection's source port so prompt and ping
    events can be correlated exactly (more precise, one more stored field
    that identifies a connection), or keep `possible-duplicate` flags and
    qualified counts only?
12. **Preview scope and clear-all.** Is refusing a stale preview (and refusing
    clear-all when newer rows exist) acceptable, or should clear-all fall
    back to bounded deletion that keeps the newer rows?
