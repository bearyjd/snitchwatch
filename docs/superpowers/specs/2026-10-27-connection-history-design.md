# Connection history (r17): opt-in storage, retention, gaps, event-time rule identity

Status: **Approved design (owner, 2026-10-27).** Design only; no code is
authorized by this document. Implementation needs its own plan, approved by the
owner. The
owner's decisions are collected in
[Owner decisions](#owner-decisions-2026-10-27); the only items still open are
two implementation-proof obligations (S1, S2).
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
| D3 | Store | One SQLite file in the bridge state directory (per-user bridge: the user state dir; system bridge: `/var/lib/snitchwatch`, service-account owned); strict 0700 directory, 0600 files, via `sqlite_file::open_owner_only` |
| D4 | What is stored | A fixed field allowlist. Command line, `process_args`, `process_env` and rule conditions on them are never stored. **Raw executable and destination strings are stored and may contain user-chosen text; this is a stated limit, not a guarantee** |
| D5 | Retention and disk | Age (default 30 days) and a 64 MiB total budget split into main-file, journal and slack shares; invariants S1-S5 (field caps, page-bounded transactions, no full VACUUM); oldest first; admission quotas against floods |
| D6 | Gaps | Session ledger; invariant: coverage never passes an uncommitted item; every discontinuity is an explicit gap; permanently unobservable traffic is labelled as such |
| D7 | Rule identity | Rule name + semantic hash + a redacted display snapshot (hash and snapshot are separate); captured by a per-prompt outcome guard for interactive decisions; `sensitive` is hashed |
| D8 | Reads | Paged, byte-bounded, bridge-wide admission, read-only opener, off the verdict path |
| D9 | Encryption at rest | Not in v1; rely on file permissions and say so |
| D10 | System bridge | **Supported in v1.** Reads only through the bridge to clients already authorised for the live stream; per-uid tagging and uid-scoped reads deferred, so on a multi-user machine every authorised client sees every user's history |

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
bridge shows "history unavailable", never an empty list. This applies to both
bridge modes: the per-user bridge (a `systemd --user` service) and the system
bridge (`packaging/system/snitchwatch-system-bridge.service`, GUI socket
`/run/snitchwatch/bridge.sock`).

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

**System bridge coverage re-checked.** The system bridge talks to the
root-run opensnitchd over a root-owned gRPC socket
(`snitchwatch-system-bridge-grpc.socket`, `SocketUser=root`) and serves GUIs
over `/run/snitchwatch/bridge.sock` (`SocketGroup=snitchwatch-ui`). The table
above holds unchanged: ping events, `nolog`, stock-vs-fork default events and
the no-GUI-prompt case are properties of the daemon and the bridge, not of
the bridge mode. Two differences: (a) the daemon sees **every user's**
connections, so history covers all users, with no per-user attribution in v1
(decision 1); (b) it is the system bridge that runs when the machine is up
with no one logged in, so its history can show activity while no GUI session
exists, but prompts still need an authenticated GUI, so those rows are
absent exactly as the table says.

## D2. Opt-in

- Off by default, on every install including upgrades. No migration turns it
  on. Nothing is created on disk until the user enables it.
- Enabling is a dedicated Settings action that states, before confirmation:
  the stored fields (D4) **and that raw program paths and destination names
  are stored**; the bridge's actual file location (reported by the bridge, D3);
  who can read it (threat model); the default retention; what is not
  observable (D1 table); how to turn it off, how to delete it, **a backup-exclude recipe for the
  reported path**, and that the data is stored unencrypted, protected only by
  file permissions.
- The enabled flag and retention settings live in bridge state, so the bridge
  never records on a stale GUI preference.
- Disabling stops recording at once, via the barrier in H02, and **keeps**
  existing history by default. "Delete history" is a separate, confirmed
  action (H02 preview and token), also offered next to the disable control.
- **Who may change it.** Per-user bridge: any authenticated client of the
  user's socket. System bridge: any client authenticated on the GUI socket
  (`snitchwatch-ui`, `SocketMode=0660`) may enable, disable and clear history;
  there is no polkit step in v1 (decision 14).
- If storage is unusable the bridge keeps running, history reports
  `unavailable: <reason>`, and it never retries into another location.

## D3. Storage location, format, permissions

- **Format:** one SQLite database. Tables: `events`, `rules_seen` (D7), `gaps`
  and `sessions` (D6), `meta` (schema version, consent record).
  `secure_delete=ON`. Chosen over JSONL because retention, counts for
  preview, and range delete need indexes and transactions.
- **Path:** the bridge's state directory, `<state>/history/history.db`. The
  Flatpak ships only the GUI; **the bridge always runs on the host**, so the
  path is a host path, never under `~/.var/app/...`:
  - **Per-user bridge** (systemd user service, `StateDirectory=snitchwatch`):
    normally `~/.local/state/snitchwatch/history/`, owned by the user.
  - **System bridge:** `/var/lib/snitchwatch/history/`. The unit runs as
    `User=snitchwatch`/`Group=snitchwatch` (created by
    `packaging/system/snitchwatch.conf.sysusers`, home `/var/lib/snitchwatch`)
    with `StateDirectory=snitchwatch` and `StateDirectoryMode=0700`, and sets
    `XDG_STATE_HOME=/var/lib`. History is therefore **owned by the
    `snitchwatch` service account, in a directory only that account (and
    root) can enter**; desktop users cannot open the file, and `snitchwatch-ui`
    group membership grants access to the bridge socket (`SocketGroup=snitchwatch-ui`, `SocketMode=0660`), not to the file; the file is 0600 `snitchwatch`.
    The existing system check (`check_system_state_dir`: owned by the service
    account and mode 0700) already applies to the parent; the history
    subdirectory adds the exact-0700 check below.
  The GUI cannot compute either path; the bridge reports the actual path in
  the history status and the opt-in and backup text use that reported value.
  A sandbox-resident writer would be a separate deployment decision and is not
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
| `src_port`, `src_ip` | Source port (decision 11) and the local source address of the connection, stored so a prompt event and a later ping event can be correlated exactly (D7). The source address is needed because two sockets can share a source port with different local addresses. NULL when the event does not carry them |
| `uid` | **Not stored in v1** (per-uid tagging deferred, decision 1) |

**Never stored, enforced by test (below):** `process_args`, the command line,
`process_env`, payloads, URL path/query, byte counters and direction (still
placeholders), the ephemeral row `id`, and **any rule condition on
`process.command` or `process.env.*`** (D7).

**Stated limit on raw strings.** `process_path`, `dst_host` and rule operands
over paths/hosts are user- or attacker-chosen text and can embed secrets
(a token in a file name, a credential in a hostname). Heuristic detection
cannot make this absolute, so the design does **not** promise "no secrets";
it promises no *fields* that routinely carry them. Reducing the exposure
further (basename plus path hash, or exclusion lists) was considered and
**deferred to v2** (decisions 3 and 9): v1 stores full paths and hosts, with
this limit stated in the opt-in text. Strings are capped at the S1 values
(`rule_name` 256 B, `process_path` 4096 B, `dst_host` 253 B, and S1 adds a cap
for every other stored string) and shown as plain text.

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

Explicit provenance on `SetVerdict` is **not added in v1** (decision 6; no
protocol change). The value is `gui` and the UI labels it **"answered in GUI
(may be silent mode)"**.

## D5. Retention, disk budget, admission

Two independent limits; whichever trips first evicts.

- **Age:** default 30 days; choices 7 / 30 / 90. No "forever".
- **Disk: invariants.** `B` is a single total budget for everything the
  feature may put on disk (default 64 MiB; choices 16 / 64 / 256 MiB; ceiling
  256 MiB). Each invariant has one enforcement mechanism and one test; where a
  number is not yet known, the implementation must prove it (see
  "Remaining implementation-proof obligations") and this design does not guess it.

| # | Invariant | Enforcement mechanism | Test |
|---|-----------|-----------------------|------|
| S1 | Every persisted field has a fixed maximum byte length, so the largest possible row, index entry and snapshot are known from the schema | One field-cap table in the insert API (`rule_name`, `process_path`, `dst_host`, rule snapshot parts and every other column); longer values are truncated with a marker or the row is refused; `page_size` is pinned (4096) at creation and verified on open | Property test: for every column, a value at cap+1 never reaches the file at more than cap |
| S2 | The rollback journal cannot outgrow its reserve, whatever a transaction writes | **Full-journal reserve (owner decision 2026-10-28; the per-row page proof was declined as unproved).** Room is reserved for a journal of every page of the main file (one 4096-byte header plus one 4104-byte record per page; `cache_spill=OFF`, verified on open, keeps it to one header), and the main file is capped at ≤ B/2. No claim is made about pages per row or rows per transaction; batch sizes only keep transactions short. The writer also checks the journal against that limit before `COMMIT` as a tripwire | Fill-to-limit workload: the main file never exceeds B/2; a transaction that journals every page keeps main + journal + settings ≤ B; mutant check: loosening the cap fails both |
| S3 | main file + maximum journal + settings ≤ B | `max_page_count` for the main file, the largest count with main ≤ B/2 and main + full journal + settings ≤ B (the leftover, about 0.2% of the main file, is never allocated); 2% of B inside the main file reserved for the ledger (D6) | Measured simultaneous file sizes under S2's worst-case workload stay ≤ B (measurement validates the invariant; it is not the enforcement) |
| S4 | Freed space returns to the filesystem without a full rewrite | `auto_vacuum=INCREMENTAL`, `incremental_vacuum(N)` in short transactions (S2 bounds the journal whatever N is). **No full `VACUUM`**: its temporary space cannot fit in B | Delete 90% of rows; file shrinks; no journal exceeds the S2 limit |
| S5 | Deleted content is not left readable in the file | `secure_delete=ON`; clear-all recreates the file (H02) instead of deleting inside it | Canary scan after delete and after clear |

  `journal_size_limit` only trims the journal after a commit and is not
  relied on for any of the above.
- **Eviction order:** per-process byte share first (admission, below), then
  oldest `ts_ms` first, in short chunks (S2). If a chunk
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
  - The numbers are the owner's decision 2 (burst sizes are implementation
    defaults); the mechanism is part of the design.

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
    The bridge therefore **merges only on verified correlation**: the stored
    `src_ip` and `src_port` (decision 11) together with protocol, destination
    address and port, and process path, within the prompt's settlement window.
    **Uniqueness is required, not assumed:** the match counts only when
    exactly one candidate exists for the whole key. If the match is ambiguous
    (more than one candidate), or `src_ip`/`src_port` is missing from either
    event, or the uniqueness proof (see Owner decisions) does not hold, **both
    events are kept and marked `possible-duplicate`, even when ports are
    present**, and any
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
blocklist subsystem to expose an applied-content revision (decision 7:
not in v1); history makes no claim about list contents.

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
delete: everything for clear and for the separate "Delete history" action; the
previewed set for a range delete; the rows the new limit would evict for a
retention change; nothing for disable (which keeps history by default). Enforcement, in this order:

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
4. **Split the queue by scope; do not discard wholesale.**
   - *Queued events* outside the scope are kept. Moving them across the
     boundary does not rewrite anything about how they were accepted: the
     queue item carries a separate **writer-generation** field, which the
     writer updates; the item's own acceptance data is immutable.
   - *Pending guards never change generation.* A guard's acceptance
     generation and consent (G1) are immutable for its whole life. A guard is
     therefore never "re-stamped": it settles only if its acceptance
     generation still equals the current one.
   - **Disable invalidates every pending guard**, including under
     disable-and-keep, whose deletion scope is otherwise empty. Each such
     guard is invalidated, a durable gap row covering its arrival is
     committed, and its pin is released in the same transaction (W1). A
     later re-enable under the same consent cannot revive it: its acceptance
     generation is older, so its outcome is discarded at settlement.
   - For clear and retention change, guards outside the scope also keep their
     original acceptance generation; whether they can still settle is decided
     by G1 against the current generation, so a control operation that
     increments the generation invalidates the guards it supersedes (each
     with its durable gap before its pin is released), and operations that
     must not interrupt recording (a rejected preview) never reach this step.
   - *Queued events and snapshots inside the scope* are discarded together.
     Any accepted item discarded outside an approved scope first gets a
     durable gap row, and only then is its pin released; a pin is never
     released by a discard alone.
5. Perform the operation (below) and acknowledge after cleanup.
6. **Invalidate** any already-produced response and GUI page of an older
   generation: responses carry the generation, the bridge refuses to deliver
   a stale one, and the GUI drops pages whose generation differs.

Tests (each with a blocked writer and a non-empty queue):
- a **retention change** keeps the queued events the new retention keeps,
  discards only those it evicts, and leaves the watermark correct;
- **disable-and-keep with prompts pending**, then re-enable under the same
  consent, then settle the old prompts: nothing is written except the gaps
  committed at disable (no event, no snapshot, no pin left);
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
  the previewed checksum still matches, **and that nothing has been accepted
  after the preview's acceptance boundary** (below); if any check fails nothing
  is deleted and a fresh preview is required (owner decision 12: refuse, no fallback). The UI reports counts and bytes only after confirmation; on
  failure it says exactly what remains. Range delete and limit reduction use
  bounded chunks obeying S2, plus snapshot cleanup.
- **Retention preview** (for clear, a range delete, or lowering a limit):
  the bridge returns `N events from A to B, about X MB` plus an **expiring
  preview token** (60 s) bound to the generation, the maximum row id, a
  checksum of the previewed set (row count, sum of row ids and of
  `repeat_count`), **and an acceptance-sequence boundary**. Every item the
  bridge accepts (queued daemon event, in-flight batch, pending guard,
  loss counter; the same set W1 pins) takes a monotonically increasing
  acceptance sequence number; the token records the highest number assigned at
  preview time. Persisted-row checks alone cannot see queued or in-flight
  items, so with a blocked writer a new event could otherwise be accepted
  after the preview, leave the checksum and maximum row id unchanged, and be
  discarded by a clear whose scope is "everything". At confirmation, under the
  control barrier and **before** any destructive step (C1 step 1), the bridge
  refuses if the current acceptance sequence is above the recorded boundary. A cutoff row id alone does not fix the set, because
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
| Same-uid processes (per-user bridge) | **Yes** | 0600 stops other users, not other processes of the user. A same-uid process can also use the socket plus token to read history through the bridge |
| Any holder of the socket path **and** token (per-user bridge) | **Yes** | History is exposed over the WS exactly as live rows are |
| Clients allowed on `/run/snitchwatch/bridge.sock` (system bridge: members of `snitchwatch-ui`; the token there is compatibility-only and socket access is the authorization boundary) | **Yes, all users' history** | Decision 1: reads go only through the bridge to clients already authorised for the live stream. This widens how **far back** they can see, not **who** can see connections. Per-uid scoping is deferred, so on a multi-user machine every authorised client sees every user's history |
| System bridge control: members of `snitchwatch-ui` (decision 14) | **Can enable, disable, clear and read** | Same audience as the live stream and prompts today; no polkit in v1. Membership is the boundary and an admin grants it; a member can erase history, so integrity is not defended against them |
| Other local users (not in `snitchwatch-ui`; not the service account) | No | System: `/var/lib/snitchwatch` is 0700 `snitchwatch`; per-user: 0700/0600 |
| root | Yes | Out of scope |
| Backups of `~` or the host state directory | **Yes** | Long-lived, often off-box. The opt-in text names the bridge-reported path so the user can exclude it |
| Other Flatpak apps | **Only if they have been granted host access** | The database is a host file owned by the user. Snitchwatch adds no permission for it, but another Flatpak granted `--filesystem=home`/`host` (or the state directory) can read it as the same uid, and any app that can reach the bridge socket and token can read history through the bridge. Snitchwatch cannot control other apps' grants; the opt-in text says so |
| Disk theft / offline access | Yes | See encryption |
| Remote | No | No listener, no upload path |

**Sensitive-data minimisation.** The D4 allowlist and the recursive snapshot
allowlist (D7) are the controls. Residual, stated exposure: raw
`process_path`, `dst_host`, the local `src_ip`/`src_port` and non-withheld rule operand values, which can
embed secrets. Mitigations on the table, not in v1 by default: basename+hash
paths and per-app / per-host exclusions are deferred to v2 (decisions 3, 9). Hashing
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
- **Allowlist test** that fails if a new field reaches the insert API
  (including `src_port`; `uid` must not appear).
- **System-bridge tests:** the file is created under the service account's
  0700 state directory with 0600 mode; a client without access to the GUI
  socket cannot read history; two authorised clients each see all rows (the
  documented multi-user behaviour); the per-user bridge keeps the per-user
  path.
- **Correlation tests:** a prompt and ping event with equal `src_ip`,
  `src_port` and the rest of the key merge; missing `src_ip` or `src_port`
  keeps both flagged `possible-duplicate`; **collision test:** two sockets from
  the same executable with the same source port, protocol and destination but
  different source IPs are never merged, and an ambiguous match (two
  candidates) keeps all events flagged.
- **Disable/keep tests:** disabling keeps history by default; "Delete
  history" requires a preview token.
- **Invariant tests** S1-S5, W1, L1, G1, C1 and C2 as tabled above, each
  adversarial (worst-case rows, kill -9, stale prompts, mid-query clear,
  newer-row-after-preview), not only happy path.
- **Budget tests** are the S1-S5 tests above; they measure main file and
  journal together as a validation of the invariants, not as their
  enforcement.
- **Flood tests:** alternating-destination flood from one process cannot evict
  another application's history beyond its quota; drops are counted.
- **Barrier tests:** blocked writer with a non-empty queue.
  - **Clear** removes exactly the approved scope: afterwards no row, snapshot
    or pin from within that scope exists.
  - **Disable** (keeps history by default) leaves existing rows and the
    accepted queued events in place, and invalidates every pending guard with
    a durable gap committed before its pin is released.
  - A preview token is refused after retention or coalescing changes the set,
    and when expired.
  - **Preview with the writer blocked:** take a clear preview, then accept
    another event into the queue; confirmation is refused (the acceptance
    boundary moved), the queued event is not discarded, and a fresh preview is
    required.
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

## Owner decisions (2026-10-27)

Resolved by the owner; each is reflected in the sections named.

1. **System bridge: allowed in v1** (reverses the earlier D10). The owner's
   installed GUI talks to the system bridge at `/run/snitchwatch/bridge.sock`.
   History is owned by the `snitchwatch` service account under
   `/var/lib/snitchwatch/history/` (D3, citing `packaging/system/`). Reads go
   only through the bridge to clients already authorised for the live event
   stream: this does not widen who can see connections, only how far back.
   **Per-uid row tagging and uid-scoped reads are deferred; on a multi-user
   machine every authorised client sees every user's history.** The per-user
   bridge keeps the per-user location. D1 coverage re-checked for the system
   bridge. (D1, D3, D4, D10, threat model.)
2. **Numbers.** 30 days; 64 MiB budget, 256 MiB ceiling; admission 200
   events/s global, 20/s per process, 25% byte share per process; field caps
   `rule_name` 256 B, `process_path` 4096 B, `dst_host` 253 B, and S1 adds a
   cap for every other stored string. (D4, D5, S1.)
3. **Exclusion lists: deferred to v2.** (D4, threat model.)
4. **Eviction: oldest first regardless of verdict.** (D5.)
5. **Daemon-side loss:** the lower-bound `upstream-loss` gap is enough for v1;
   no daemon changes. (D1, D6.)
6. **Provenance:** no protocol change in v1; the UI says "answered in GUI (may
   be silent mode)". (D4.)
7. **List-backed rules:** keep the "list contents not recorded" qualifier; no
   blocklist revision in v1. (D7.)
8. **Encryption: none in v1.** Rely on the 0600/0700 permissions and say so in
   the opt-in text. (D2, D9, threat model.)
9. **Raw paths and hosts:** stored in full, with the limit stated. (D4.)
10. **Backups:** the opt-in text names the bridge-reported path and gives a
    backup-exclude recipe. **Disabling keeps history by default**; "Delete
    history" is a separate, confirmed action (reverses the earlier default).
    (D2, H02.)
11. **Source port: stored**, used for exact prompt-to-event correlation. For
    exactness the correlation key also includes the source address, so the
    local source address is stored with it (the port alone does not identify
    a socket; the owner confirmed storing `src_ip` on 2026-10-27). The `possible-duplicate` path
    applies wherever the key is missing or the match is ambiguous, not only
    when the port is missing. Added to the D4 allowlist and the threat model.
    (D4, D7.)
12. **A stale preview is refused and a fresh one required; clear-all refuses
    if newer rows exist.** (H02, C2.)

13. **System bridge storage, verified from `packaging/system/`.** The unit
    `snitchwatch-system-bridge.service` runs `User=snitchwatch`,
    `Group=snitchwatch` (lines 15-16) with `StateDirectory=snitchwatch` and
    `StateDirectoryMode=0700` (lines 53-54), which is `/var/lib/snitchwatch`.
    History lives under `/var/lib/snitchwatch/history/`, owned by
    `snitchwatch`, with a 0600 database file. The GUI socket is
    `snitchwatch-system-bridge-gui.socket`: `ListenStream=/run/snitchwatch/
    bridge.sock`, `SocketGroup=snitchwatch-ui`, `SocketMode=0660` (lines
    7-10). (D3, threat model.)
14. **Control and read access on the system bridge.** Any client
    authenticated on the GUI socket, meaning a member of `snitchwatch-ui`, can
    **enable, disable and clear history, and read it.** This is the same
    audience that already receives the live stream and can answer prompts. No
    polkit in v1. **`snitchwatch-ui` membership is the security boundary, and
    an administrator grants it**; anyone with it can also erase history, so
    history is not tamper-resistant against them. (D2, H02, threat model.)

### Remaining implementation-proof obligations (not owner decisions)

- **S1:** the implementation must derive the field-cap table for every stored
  string beyond the three the owner set, and prove from the schema that no
  value can exceed it.
- **S2:** resolved by the owner (2026-10-28): the per-row page proof was
  declined as unproved, and the full-journal fallback is used: a full
  rollback journal is reserved and the main file is at most half of the
  budget. No per-row page constant is claimed.
- **Correlation uniqueness:** the implementation must prove that `src_ip`,
  `src_port`, protocol, destination and process path identify one live socket
  within the settlement window; where it cannot, or the match is ambiguous,
  events are kept and flagged.
