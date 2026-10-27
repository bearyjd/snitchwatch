# Connection history (r17): opt-in storage, retention, gaps, event-time rule identity

Status: **PROPOSED, awaiting owner approval.** Design only; no code is
authorized by this document. Implementation waits for the owner to resolve the
[open questions](#open-questions-for-the-owner) and approve.
Baseline: main `c4430bd` (2026-10-27).
Roadmap items: **H01** (opt-in persistent allowed/denied history bounded by
age and disk size), **H02** (clear history, retention preview, storage
health), **H06** (preserve matched-rule ID/class and revision at event time),
serving **R05** (explain "why allowed/blocked?"). Source rows:
[competitive feature roadmap](2026-10-24-competitive-feature-roadmap.md)
lines for H01, H02, H06, R05; plan item 8 of
[the overnight run](../plans/2026-10-27-overnight-autonomous-run.md).
Later items that depend on this (H03 filters, H05 baselines, H08 digests, Q07
budgets) are out of scope here.

## Decisions requested

| # | Decision | Recommendation |
|---|----------|----------------|
| D1 | Who writes history | The **bridge**, not the GUI |
| D2 | Default | **Off.** Enabled only by an explicit user action |
| D3 | Store | One SQLite file in the bridge state directory, 0600 in a 0700 directory |
| D4 | What is stored | A fixed minimal field list; `process_args`, `process_env`, command line, and byte counters are never stored |
| D5 | Retention | Age (default 30 days) **and** size (default 64 MiB hard cap); oldest events evicted first |
| D6 | Gaps | Explicit gap records, shown in the UI, never silent |
| D7 | Rule identity | Rule name + SHA-256 of the rule as delivered with the event, plus a deduplicated rule snapshot |
| D8 | Encryption at rest | Not in v1; rely on file permissions and say so in the UI |
| D9 | Multi-user (system bridge) | **Not supported in v1**; history is disabled there |

## What exists today (grounding)

- **Events.** opensnitchd dials the bridge as a gRPC client. The bridge
  translator turns each connection event into a `ConnectionRow`
  (`crates/snitchwatch-bridge/src/translator/connection.rs`,
  `ws_messages/verdict_wire.rs`) with fields `id, process, process_path,
  dst_host, dst_ip, dst_port, protocol, direction, action, bytes_sent,
  bytes_received, started_at_ms, matched_rule, auto_answer`. The daemon event
  also carries `process_args` and `process_env`; the translator does not copy
  them into the row (the row-field list is pinned by a test in
  `analysis_export/snapshot_tests.rs`).
- **Memory only.** `ConnectionCache` (`cache/connections.rs`) keeps the most
  recent rows (default capacity 10,000), evicting the oldest non-pending rows,
  with no coverage ledger. A restart of the bridge empties it. The GUI model
  holds what the bridge replays.
- **Row caveats that history inherits.** Byte counters are placeholders and
  direction is hardcoded outgoing (see the
  [export design](2026-10-24-ai-ready-cache-export-design.md), "Existing
  evidence"). Prompt rows are stamped with bridge construction time; decided
  rows use daemon event time. History must not present these as measured
  sessions.
- **Matched rule today.** `matched_rule` is the rule *name* only
  (`row.matched_rule = (!by_default).then(|| rule.name.clone())`). A
  default-action event has none. `InspectorPanel.qml` already shows the
  matched rule and "Show rule" navigation; that resolves the name against the
  *current* rule list, so an edited or deleted rule rewrites the explanation.
  This is the R05 gap that H06 closes.
- **Local persistence precedents.**
  - Learning mode (`crates/snitchwatch-kirigami/src/learned.rs`,
    `paths.rs::learned_path`): GUI-side, under `$XDG_CONFIG_HOME`, mode 0600,
    entries deleted after 30 days, an advisory lock file beside it, bounded by
    `SEEN_CAP`.
  - Bridge state (`snitchwatch-bridge-cli/src/storage.rs`): the state
    directory is resolved once, must be 0700 and owned correctly, SQLite holds
    blocklists and profiles, and a storage problem never stops the bridge: it
    degrades to memory-only and is shown to the GUI (`storage` status).
  - Export design: "persistent history/retention" was explicitly deferred to
    this document; multi-user/UID authorization was left open.
- **Access boundary today.** Any authenticated WS client reads all live rows;
  the boundary is the Unix socket (0700 directory, 0600 socket) plus the
  handshake token. History adds no new authentication, so it inherits the
  same boundary and must not widen it.

## D1. Where history is written

**Recommendation: the bridge writes, the GUI reads over the existing WS.**

Reasons: the bridge sees every event, including those while the GUI is closed;
it already owns durable state and the degrade-to-memory pattern; and it knows
the full `Rule` delivered with the event (needed for D7). A GUI-side store
would lose every event while the GUI is not running, which is the dominant
case for a background firewall, and would make gap labelling unprovable.

Cost: a new WS request/response pair (query, preview-delete, clear, status)
and a capability flag so an older bridge shows "history unavailable", never an
empty list. Same compatibility rule as the export design.

Rejected: GUI-side store (loses events when closed; see Alternatives).

## D2. Opt-in

- Off by default, on every install, including upgrades. No migration turns it
  on. Nothing is created on disk until the user enables it.
- Enabling is a dedicated action in Settings that states, in plain text before
  confirmation: what is recorded (the field list in D4), where the file is,
  who can read it (D8/threat model), the default retention, and how to turn it
  off and delete it. The confirmation is the consent record; the enabled flag
  and the retention settings live in the bridge's state, not only in a GUI
  setting, so the bridge never records on a stale GUI preference.
- Disabling stops recording at once. It does **not** delete existing data
  silently: the UI offers "Disable and delete history" and "Disable and keep".
  The default button is delete (privacy-preserving).
- If storage is unusable (permissions, disk full, read-only), the bridge keeps
  running, history reports `unavailable: <reason>`, and the UI says so. It
  never retries into a different, less safe location.

## D3. Storage location and format

- **Format:** one SQLite database (WAL off; `synchronous=NORMAL`; `secure_delete=ON`),
  matching the bridge's existing SQLite use. Tables: `events`, `rules_seen`
  (D7), `gaps` (D6), `meta` (schema version, enabled-at, last clean shutdown).
  Chosen over JSONL because retention by size and age, clear-all, and the
  deletion preview need indexed range deletes and counts.
- **Path:** `<bridge state dir>/history/history.db`.
  - Per-user bridge: `$XDG_STATE_HOME/snitchwatch/history/` (fallback
    `~/.local/state/snitchwatch/history/`). State, not config: it is
    regenerable-in-principle log data, and backup tools that sweep
    `~/.config` should not pick it up by default.
  - Flatpak: the same XDG variables resolve inside the sandbox to
    `~/.var/app/<app-id>/.local/state/snitchwatch/history/`. This path is
    private to the app: other Flatpaks cannot read it without a granted
    `--filesystem` permission, but the host user and any host process of the
    same uid can. The Flatpak manifest must **not** add a filesystem
    permission for it. The runtime socket permission in the Flatpak decision
    (`xdg-run/snitchwatch`) is unrelated and unchanged.
  - System bridge (`/var/lib/snitchwatch`): unsupported in v1 (D9).
- **Permissions:** directory 0700, files (`history.db`, `-wal`/`-shm` if ever
  enabled, journal) 0600, set at creation with a restrictive umask so there is
  no window of wider mode. Reuse the state-directory check in `storage.rs`
  (owner, mode, not a symlink); refuse to open if it fails. Re-check mode on
  every open.
- **Schema versioning:** `meta.schema` integer; the bridge refuses a newer
  schema it does not understand rather than guessing, and reports
  `unavailable: newer schema`.

## D4. What is stored (minimum by default)

Per event, exactly:

| Field | Notes |
|-------|-------|
| `ts_ms` | Event time. For prompt rows decided later, the decision time is a separate `decided_ts_ms`; the construction-time stamp is not used |
| `process_path` | Absolute executable path as reported (may be absent: stored as NULL, not empty) |
| `dst_host`, `dst_ip`, `dst_port`, `protocol` | Destination as seen. `dst_host` is the most sensitive field after the path; see threat model |
| `action` | allow / deny / blocklist / default-allow / default-deny / pending-unanswered |
| `rule_name`, `rule_hash` | D7. NULL for default-action events |
| `answered_by` | `rule`, `default`, `gui-prompt`, `timeout`, `silent-mode`, from the existing `auto_answer` and default-action marker |
| `uid` | Only if D9/open question 1 is approved; otherwise not stored |

**Never stored, enforced by an allowlisted insert API and a test that fails if
a new field is added without review:**

- `process_args` and the full command line (routinely contain tokens, URLs
  with credentials, file names);
- `process_env` (secrets);
- payload, URL path or query, TLS SNI beyond what `dst_host` already is;
- byte counters and direction (currently placeholders; storing them would
  assert measurements the system does not make);
- the ephemeral row `id` (not meaningful across runs; an autoincrement key is
  used instead).

Strings are stored as received, length-capped (path 4096, host 255), and
treated as untrusted text everywhere they are read back (plain text in the UI,
never rich text), as in the export design.

## D5. Retention and bounded growth

Two independent limits; whichever trips first evicts.

- **Age:** default 30 days (same as learning mode's precedent); user choices
  7 / 30 / 90 days. No "forever" option in v1.
- **Size:** hard cap on the database's on-disk size (main file plus any
  journal), default 64 MiB, user choices 16 / 64 / 256 MiB, ceiling 256 MiB.
  A cap is enforced, not advisory.

Mechanics:

- **Eviction order:** oldest `ts_ms` first. Allowed and denied are evicted
  equally; no verdict is kept longer than another in v1 (see open question 4).
  Gap records and `rules_seen` entries are evicted with the events they
  describe (a gap older than the oldest remaining event is merged into a
  single "history before X was removed" marker, D6).
- **Enforcement point:** before each batch insert, the bridge checks
  `page_count * page_size` against the cap and deletes the oldest N events in
  the same transaction when over 90% of it; a background pass applies the age
  limit hourly and at startup. `PRAGMA max_page_count` is set to the cap as a
  last-resort backstop so a bug cannot grow the file past it. After large
  deletes the bridge runs `VACUUM` off the hot path (it needs temporary space
  up to the DB size; if there is not enough free disk, it skips and reports
  storage health instead of failing).
- **Insert path is bounded and never blocks decisions.** Events are queued in
  a bounded channel (default 4,096) and written in batches. If the queue is
  full, the event is dropped and counted into a gap (D6). A slow or failing
  disk must never delay a verdict reply to opensnitchd.
- **Disk full / write error:** recording pauses, a gap starts, health shows
  `write failing: <reason>`, and the bridge retries with backoff. It never
  deletes unrelated files to make room.
- **Rate cap:** a runaway process producing millions of connections cannot
  evict a week of history in minutes. Per-process-path coalescing: identical
  consecutive events (same path, destination, port, protocol, action, rule
  hash) within 1 s increment a `repeat_count` on the previous row instead of
  adding a row. The count is exact; only the timestamps collapse.

## D6. Gap and loss labelling

History is only honest if its holes are visible. A `gaps` row records
`start_ms`, `end_ms`, and a `cause`:

| Cause | When recorded |
|-------|---------------|
| `bridge-down` | Detected at startup: `meta.last_clean_shutdown` absent or older than the last event; gap runs from the last stored event time (or last heartbeat) to startup |
| `daemon-disconnected` | opensnitchd stream closed (the bridge already tracks `daemon_down` and a generation counter); the interval until the next HELLO |
| `logger-drop` | opensnitchd/logger reported or the bridge detected a sequence discontinuity or lag (the cache already notes broadcast lag, `Lagged`); count of missed events if known, else unknown |
| `queue-overflow` | History queue was full (D5) |
| `write-failed` | Storage error (D5) |
| `disabled` | Recording was off; makes "user turned it off for a week" a labelled interval, not an unexplained hole |
| `evicted` | Retention removed events; the earliest remaining time and removed count are shown ("History before 12 Oct was removed by the 30-day limit") |
| `clock-jump` | Wall clock moved backwards or by more than a threshold; ordering of events in that interval is unreliable |

Rules:

- A heartbeat row (`meta.last_alive_ms`, updated every 30 s) bounds the
  `bridge-down` interval so a crash does not imply coverage up to the crash
  instant. The gap reads "between 14:02 and 14:31, exact start unknown".
- Gaps never claim to know what happened inside them ("nothing was
  connected") and the UI never shows an empty interval without a gap or an
  explicit "no events recorded while history was on" statement (which is only
  valid when `meta` shows continuous recording).
- **GUI closed is not a gap.** The bridge records regardless. What the GUI
  closed loses is live prompts, which is an existing behaviour; history shows
  `answered_by` for those events (`timeout`, `default`).
- Opensnitch event loss upstream of the bridge (daemon-side logger drops) is
  not directly observable. The design says so and labels the whole history
  "events the bridge received", with the daemon-reported drop counters
  recorded as `logger-drop` when available. Open question 5 asks whether to
  pursue daemon counters.
- Exports and any later digest (H08) must carry gap and coverage metadata;
  a summary computed over a range with gaps states the covered fraction.

## D7. Event-time rule identity (H06) for R05

Goal: answer "why was this allowed or blocked?" as of the event, even if the
rule has since been edited or deleted.

At event time the bridge already holds the full `Rule` message delivered with
the daemon event. Record, from that message and not from a later lookup:

- `rule_name` (as today);
- `rule_hash`: SHA-256 (truncated to 128 bits, hex) over a canonical encoding
  of the semantic fields: name, action, duration, enabled flag, precedence,
  nolog, and the operator tree (type, operand, data, sensitive, list), with
  fields in a fixed order and no timestamps. A change to any matching-relevant
  field changes the hash; renaming does too (name is part of it).
- A deduplicated `rules_seen(rule_hash, canonical_json, first_seen_ms)`
  snapshot, so the UI can show what the rule said then. Bounded: at most 2,000
  entries; the oldest unreferenced snapshots are evicted with the events that
  referenced them. The hash remains on the event if the snapshot was dropped,
  and the UI then says "rule definition no longer retained".
- No hash for default-action events; `answered_by = default` explains them.

UI behaviour (R05): the inspector shows the recorded name and, when the rule
still exists with the same hash, "unchanged"; if the current rule has a
different hash, "this rule has changed since" with the recorded snapshot
beside the current rule; if gone, "this rule has been deleted" plus the
snapshot. Editing or deleting a rule never rewrites stored events.

Notes: the roadmap's "ID/class and revision" is realised as name + content
hash, since opensnitch rules have no stable numeric ID or revision counter;
the hash *is* the revision. Rules loaded from files can share a name across
reloads; the hash disambiguates. Class (user rule / curated default /
blocklist) is derived from the rule-name prefix conventions already used
(`snitchwatch-default-*`, `z00-blocklist:*`) and stored as `rule_class`.
The existing R05 matched-rule display is preserved and is not re-sold as new.
The rule snapshot contains the user's own match patterns (paths, hosts,
regexes), which is the same sensitivity class as `dst_host`; it is covered by
the same clear-all and retention.

## H02. Clear, retention preview, storage health

- **Clear all** deletes events, gaps, `rules_seen`, and `meta` history
  markers in one transaction, then runs `VACUUM` and truncates any WAL/journal
  so deleted pages are not left in free space. Because of `secure_delete=ON`
  deleted content is zeroed on page reuse; `VACUUM` rewrites the file. The UI
  shows the result ("Deleted 18,402 events, 3.1 MB") only after the bridge
  confirms success; on failure it says exactly what remains.
- **Retention preview:** any change to age or size settings, and "Clear",
  first asks the bridge for a dry-run count: "This will delete N events from
  A to B and free about X MB." Computed with an indexed `COUNT`/`SUM(length)`
  estimate, labelled approximate for bytes and exact for counts. Nothing is
  deleted until confirmed. Lowering a limit shows the preview before it
  applies.
- **Per-range delete** (e.g. "older than 7 days") uses the same preview.
- **Storage health** (one status object): enabled, path, size vs cap, event
  count, oldest event, last write, queue drops, current gap (if any), last
  error. The Settings page shows it; a failing store raises a visible banner,
  not a log line.
- **Deletion guarantees, stated honestly in the UI:** deletion removes data
  from the database file and, with `VACUUM`, from its free pages. It does not
  and cannot reach copies already made by backups, snapshots (btrfs/LVM), disk
  journals, SSD wear-levelling remnants, or exports the user saved. The
  confirmation text lists these.
- History is excluded from the AI-ready cache export in v1 (the export reads
  the in-memory cache only). Any future export of history is a separate design.

## Threat model

**Assets.** A time-ordered record of which program talked to which
destination and when. That reveals browsing and usage habits, VPN/Tor use,
software inventory with versions in paths, work patterns (when the machine is
in use), and, via `dst_host`, potentially sensitive sites. It is a profile of
the user.

**Who can read it.**

| Reader | Can read? | Notes |
|--------|-----------|-------|
| Same-uid processes (any app the user runs, including compromised ones) | **Yes** | 0600 stops other users, not other processes of the same user. A malicious same-uid process can also read the live WS data via the socket token file, so history does not widen the boundary but does add a durable, larger target |
| Other local users | No | 0700/0600; the system bridge's shared case is the reason for D9 |
| root | Yes | Out of scope |
| Backups of `~` or `~/.local/state` | **Yes** | Often off-box and long-lived. Mitigation: state (not config) path; document an exclude entry; Flatpak data is under `~/.var` which many tools do include. Surfaced in the opt-in text |
| Other Flatpak apps | No (unless granted `--filesystem=home` etc.) | Holds only if we ship no extra permission |
| Disk theft / offline access | Yes | See encryption trade-off |
| Remote | No | No network listener; no upload path exists in this design |

**Sensitive-data minimisation.** The allowlist in D4 is the control: no args,
no env, no payload, no URL path. Residual leakage: `dst_host` and
`process_path` (home-directory paths can include user names or project
names). Mitigations considered: hashing hostnames (kills the main use of the
history, "what did X talk to"); a per-host exclusion list (cheap, useful, open
question 3); a "don't record" toggle per application (same). Neither is in v1
by default; the opt-in text names the exposure.

**At-rest encryption trade-off.** Options: (a) none, rely on permissions and
disk encryption (recommended v1, matches the learned/profile files); (b)
SQLCipher with a key in the Secret Service/KWallet; (c) a key derived from a
passphrase. (b) protects against casual disk/backup exposure but not against
a same-uid process (it can ask the keyring too), needs a keyring that may be
locked or absent on atomic/headless setups, adds a C dependency to a
reproducibly built release, and makes data loss on keyring reset likely. (c)
requires a prompt on every bridge start, which the background service cannot
do. Net: (a) now; revisit (b) if the owner wants backup-exposure protection,
because that is its only real gain. The UI must not say "encrypted" or
"secure".

**Integrity.** A same-uid attacker can edit history. Not defended; history is
a convenience log, not forensic evidence, and the UI does not call it an audit
log. Tamper-evident hashing chains are listed as an alternative, rejected.

**Injection and untrusted text.** Process paths and hostnames are
attacker-influenced. All SQL uses bound parameters; the schema has no dynamic
SQL; reads return plain text; the UI shows them with plain-text format, as in
the export design.

**Resource abuse.** Covered by D5 (bounded queue, coalescing, hard cap,
`max_page_count`). A flood cannot grow the disk past the cap or stall
verdicts.

**Deletion guarantees.** See H02. Not guaranteed against copies outside the
file.

**Privacy of the consent.** The enabled flag lives in bridge state so a GUI
bug or stale preference cannot start recording.

**Failure behaviour.** Closed: storage failure means not recording and
saying so; never recording to a fallback path.

## Alternatives considered

1. **GUI-side history** (like learning mode). Simple, no bridge protocol, but
   loses all events while the GUI is closed, so coverage claims are false for
   most of the day. Rejected.
2. **Rely on opensnitchd's own logging** (its `loggers` / syslog / remote
   sinks, and its optional SQLite). Upstream, but it records args/env-adjacent
   fields depending on config, is outside Snitchwatch's retention and clear
   controls, cannot carry the rule snapshot, and cannot label bridge-side
   gaps. Could be a future input; not the store.
3. **JSONL append files with rotation.** Easy and greppable, but age/size
   retention, counts for preview, and range delete become file rewrites, and
   crash-torn lines need handling. Rejected for SQLite.
4. **Hash hostnames at rest.** Shrinks the threat but destroys "what did this
   program contact", the main purpose. Rejected as default; possible
   per-host exclusion instead.
5. **Encrypt with SQLCipher or per-record keys.** See threat model; deferred.
6. **Store full rule history as a rule revision log** (every edit). Heavier,
   needs rule-edit hooks in all writers. Event-time hash + snapshot gives the
   R05 answer without it.
7. **Tamper-evident chain.** Overkill for a convenience log; rejected.
8. **Keep history forever / archive.** Contradicts bounded growth. Rejected.

## Verification plan (for the later implementation, not done here)

- Property tests: size never exceeds the cap under adversarial insert rates;
  age eviction exact; coalescing counts exact.
- A test that fails if any field outside the D4 allowlist reaches the insert
  API (mirrors the row-field pin in `snapshot_tests.rs`), including a canary
  `process_args`/`process_env` that must not appear in the file bytes.
- File-mode tests (0700/0600, symlink refusal, wrong-owner refusal) on the
  bridge-cli storage pattern; Flatpak path resolution test.
- Crash-injection tests for gap detection (kill -9, daemon disconnect, full
  queue, full disk).
- `mock_opensnitchd` scenarios for event-time rule identity: edit and delete
  the rule after the event; stored explanation unchanged. Per repo convention
  never against a live opensnitchd.
- Clear-all: after clear, a byte scan of the DB file for canary host/path
  strings finds none.
- A GUI test for each gap cause label and for "unavailable" (older bridge).

## Open questions for the owner

1. **Multi-user.** The system bridge sees all users' events. v1 disables
   history there (D9). Do you want per-uid rows with uid-scoped reads
   (needs an authorization design), or keep history per-user-bridge only?
2. **Defaults.** Are 30 days and 64 MiB the right defaults, and 256 MiB the
   right ceiling?
3. **Exclusions.** Add a per-application and per-host "don't record" list in
   v1 (recommended, small), or defer?
4. **Eviction bias.** Evict oldest-first regardless of verdict (proposed), or
   keep denies longer than allows?
5. **Daemon-side loss.** Pursue opensnitchd drop counters to make
   `logger-drop` exact, or label as "unknown" only?
6. **Encryption.** Accept "none, rely on permissions" for v1, or require the
   keyring-backed option before shipping?
7. **Backups.** Should the opt-in text name a specific exclude recipe, and
   should the Flatpak mark its state directory as backup-excluded where
   possible?
8. **Clear on disable.** Confirm that "Disable and delete" is the default
   action of the disable dialog.
9. **Process path.** Store the full path (proposed) or only the basename plus
   a path hash for privacy, at the cost of ambiguous matches?
