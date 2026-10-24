# Log export for deterministic and probabilistic analysis (DRAFT, for r17)

Status: draft plan, no code. Release: r17 (owner roadmap 2026-10-22; r16 is eBPF).
Needs the owner's remaining answers (below) and a security-reviewer pass before
any implementation, because it persists and moves connection metadata.

## Goal
Let the owner (or a tool) analyse what the firewall did, to find issues and
patterns: why unmatched prompts keep arriving, which programs are noisy, what a
new rule would cover, whether feed lag correlates with traffic bursts.

## Where we are today (main bfc76e6)
- Bridge logs with `tracing_subscriber::fmt()` at `info` by default
  (`snitchwatch-bridge-cli/src/main.rs:87`, `EnvFilter`, `RUST_LOG=info` in
  `packaging/systemd/snitchwatch-bridge.service:40`). Output is free text in the
  journal. The "nobody answered" line now carries path + destination (#186,
  shipped in r15); it has no uid because `ConnectionRow` has none and the bridge
  never reads the daemon's `Connection.user_id`.
- The connection cache already holds structured rows (`ConnectionRow`:
  process, process_path, dst_host/ip/port, protocol, action, matched_rule,
  auto_answer, timings). That is a better analysis source than parsing logs.
- There is a diagnostics report path (`DiagnosticsReport`) for health checks.
- The GUI runs in a sandbox whose journal the tower cannot read, so GUI-side
  events (for example feed lag) must be reported to the bridge or shown in-app.

## Two layers, built in this order

### 1. Deterministic export (build first)
A read-only export of recent connection rows and events as newline-delimited
JSON, plus a small summary: counts by process, destination, verdict, rule class
(see Privacy: names are redacted), auto-answer reason; unmatched-prompt rate per minute; repeat intervals per
(process, destination); feed-lag and re-broadcast counts.
- Export goes through a dedicated, allowlisted export DTO (not a serialization of
  `ConnectionRow` or the WS messages), so new fields are dropped by default. v1
  event types: connection rows, the summary counters, and feed-lag/re-broadcast
  counts. Profile/network identity, rule names and descriptions, rule operators and
  notices are NOT exported. A test enumerates every `ConnectionRow` field and fails
  when one is not classified (kept / hashed / class / dropped) at each level.
- Per-row fields kept at `hosts-hashed` and `none` (aggregated only at
  `summary-only`): timestamps (`started_at_ms`), ports, protocol, direction and
  verdict. Byte counts are included only when genuine counters become available;
  today's translator supplies placeholder zeroes, which must be exported as
  unavailable rather than measured zero. The `none` warning lists the included
  fields too. Housekeeping fields
  `id`, `answer_deadline_ms`, `deferred` and `decided_by_default` are kept as-is
  (a counter and timestamps/booleans; no user data).
- Stable schema with a `schema` version field. Same input gives the same output
  (sorted keys, fixed timestamp format) so it can be tested with golden files.
- Source: the connection cache and counters, not log scraping. Logs stay for
  humans; add structured fields to the few log lines analysis needs.
- Delivery: ONE path. The GUI sends a new WS request over the authenticated
  socket; the bridge builds and redacts the export and returns it; the GUI writes
  the file with the user's own permissions (GUI side: `private_files::write_private`
  in `snitchwatch-kirigami`). The bridge never writes to a user-named path and
  never makes a network call. There is no CLI subcommand in v1.
- The bridge's own on-disk history (below) needs a NEW bridge-side append helper:
  `snitchwatch-bridge/src/state_file.rs` only has read and an atomic replace
  (temp file + rename), and the append helper `open_private_append` lives in the
  GUI crate (`snitchwatch-kirigami/src/private_files.rs`), which the bridge must
  not depend on. Reuse its checks (owner, no symlink, 0600), written for the bridge.
- Tests: golden-file test with fixture rows (use `MockOpensnitchd` per
  CLAUDE.md, never a live daemon); redaction tests (below).

### 2. Probabilistic analysis (separate, later)
Pattern clustering and "what is probably happening" guesses over the
deterministic export. Runs OUTSIDE the bridge, on the exported file:
- an offline script that uses only the summary (counts, intervals), or
- an LLM pass the owner starts by hand on an export they have reviewed.
The bridge never calls out to a model. Probabilistic output is advisory and
labelled as such; it never edits rules.

### AI-ready export package (owner-approved feature, 2026-10-24 UTC)

The owner explicitly requested export for analysis with Claude, ChatGPT/OpenAI,
or models accessed through OpenRouter, then approved adding it to the roadmap.
This approves the feature direction; implementation still needs a design and the
review described below. It does not authorize automatic uploads or rule changes.

- Start with a manual, provider-independent download: versioned JSON/JSONL,
  a human-readable summary, a field dictionary, and an optional CSV projection.
  The package includes an analysis prompt the user can copy into their chosen
  tool. No API key or paid provider account is required to export.
- Let the user choose a time window and app/domain/verdict filters, then preview
  the export and its redaction level. Preserve the existing default
  `summary-only` policy and the allowlisted DTO; optional fields must not bypass
  the privacy rules below. Matched rules use class only at `summary-only` and
  `hosts-hashed`; raw rule names appear only at `none` after the existing
  per-export warning.
- Where observed, include connection counts, first/last-seen times, verdicts,
  durations, auto-answer reasons, and rule matches. Describe sampling, truncation,
  missed events, unavailable fields, and time-zone information. Do not invent
  byte counts, complete session history, or attribution certainty.
- Ask the model to explain noisy apps, repeated denies, unfamiliar destinations,
  changes from a baseline, and possible rule simplifications. Every finding
  should cite event/group identifiers, distinguish evidence from speculation,
  and describe what further evidence would confirm it.
- Treat domain names, process names, and rule text as untrusted data in the
  analysis prompt. Model recommendations are a reviewable report; importing or
  applying any resulting policy is a separate, explicit action.
- Later, separately scoped options: user-triggered provider adapters for
  Anthropic/OpenAI/OpenRouter, local-model analysis, and scheduled reports.
  Provider routing, credentials, costs, retention, and upload consent need their
  own design; they are not requirements for the first export release.

Acceptance: export a fixture-selected time window reproducibly; preview the exact
fields before saving; retain stable references within the file; pass redaction
and truncation tests; and use the same file with different AI tools without
changing firewall policy or sending data from the bridge.

## Privacy and security (why this needs review)
The data names programs, hosts, IPs and local usernames.
- Exports are off until the user triggers one, per export. No background upload.
- History (the rolling raw-row store) is OFF until the owner enables it in
  settings. It holds plaintext raw rows (hostnames, IPs, paths) protected only by
  0600/0700 and whatever disk encryption the host has. "Clear history" unlinks and
  overwrites the segments on a best-effort basis (not a guarantee on SSDs or
  copy-on-write filesystems); say so in the UI.
- Redaction levels, each defined by what it CONTAINS:
  * `summary-only` (default): aggregate counts and intervals only, keyed by
    process basename (only for allowlisted paths, else its keyed hash) and a keyed
    hash of the destination. No raw rows, hostnames,
    IPs or paths.
  * `hosts-hashed`: raw rows with hostnames and IPs replaced by a keyed hash;
    paths scrubbed (below); ports, protocol, verdict kept; the process basename is kept only for allowlisted
    paths and hashed otherwise.
  * `none`: raw rows. Only after the user accepts a per-export warning that names
    what it contains (process paths, hostnames, IPs, local usernames).
- Hashing: HMAC with a random per-export key that is generated for the export and
  DISCARDED (never written to the file), so a reader cannot dictionary-test common
  hostnames against the hashes, and two exports cannot be linked by hash. Values
  are stable within one export. Trade-off: hashes cannot be compared across
  exports, so "which destination is noisy over time" is only answerable within one
  export at the redacted levels; an owner-held persistent key (0600) would allow
  cross-export comparison but makes a leaked export dictionary-testable. Default:
  discard (open question 2).
- Path handling is an ALLOWLIST, not a scrub list. At `summary-only` and
  `hosts-hashed`, keep a path only if its NORMALIZED form (no `..`, no `//`, no
  trailing ` (deleted)`) starts with `/usr/`, `/opt/`, `/app/`, `/bin/` or
  `/sbin/`; anything that fails normalization is treated as not allowlisted. Every other path (home directories incl. `/var/home` and
  `/home`, `/root`, `/var/roothome`, `.var/app`, `/run/media/<user>`,
  `/run/user/<uid>`, `/var/mnt`, `/tmp`, volume labels) becomes
  `<path>:<keyed hash of the basename, 8 hex>`; the raw basename is not kept
  either, because it can carry user text. The same applies to the row's `process`
  field (the raw comm/basename, `verdict_wire.rs` `ConnectionRow.process`): for a
  non-allowlisted path it is replaced by the same keyed hash, so a script name in
  a home directory cannot leak through it. Process args, env and cwd are never
  exported. A fixture with each path form above must pass.
- Rule names are data too: `ConnectionRow.matched_rule` for once-off rules is
  built from the host and process path (`translator/verdict.rs::rule_name_for`,
  `snitchwatch-allow-<host>-<port>-p<program>`). At `summary-only` and
  `hosts-hashed`, replace `matched_rule` by its class: `once-off` (name starts
  `snitchwatch-allow-`/`snitchwatch-deny-`, i.e. `is_answer_name`), `recommended`
  (name starts `snitchwatch-default-`, `rule_name.rs` `CURATED_DEFAULT_RULE_NAME_PREFIX`), `user-rule` (any other named rule), `default-action`
  (decided by the daemon default; unrelated to the `snitchwatch-default-` prefix), `unmatched` (no rule). Only the `none` LEVEL
  keeps the raw name. A
  fixture with a `snitchwatch-allow-example.com-443-p...` name must show the host
  absent from the output at both redacted levels.
- The exported file records its level (`redaction` field).
- Access model (honest limits): the per-export warning is a consent control, not
  a security boundary, because any authenticated socket client can already read
  every live row. The real boundary is who can connect to the bridge's GUI socket:
  `/run/snitchwatch/bridge.sock`, owner root, group `snitchwatch-ui`, mode 0660
  (`packaging/system/snitchwatch-system-bridge-gui.socket`). The opensnitchd gRPC
  socket is the root-only one (0600); do not conflate them. So every member of
  `snitchwatch-ui` can read history and request any export level. Before history
  is enabled the owner must confirm that group's members are trusted, or enabling
  history and the `none` level must be restricted to uid 0 (open question 1). Per-user scoping on a multi-user host needs a uid on `ConnectionRow`
  (a row/wire change, not in v1); the WS server already knows each client's peer
  uid (`ws_server.rs:161-196`). Record this as a known limit in the user docs.
- security-reviewer pass on the request, the redaction, the history store and the
  consent flow before code.

## Decided (owner, 2026-10-22)
- v1 is a manual export: the owner triggers it and gets a file, then drops it
  wherever they want (to an AI by hand, a ticket, a share). No background export,
  no upload, no bridge-to-model call in v1.
- Auto-export and auto-analyze are roadmap items for later, not v1. They need
  their own plan: a schedule, a destination, retention and a stronger privacy
  review, because the data would then leave the machine without a per-export
  decision.
- Redaction is ON by default; a full (unredacted) export is possible only after
  the user explicitly accepts a warning for that export (per export, not a saved
  preference).
- Where it lives: a Kirigami "Export" action; the GUI asks the bridge, the GUI
  writes the file (see Delivery above).
- Retention: a rolling window on disk, bounded by size AND age (whichever hits
  first; starting values to tune after measuring: ~32 MiB, ~14 days), as rotating
  append-only segment files, owner-only (0600 file in a 0700 dir). Raw rows are
  stored so a full export is possible; redaction happens at export time. Needs a
  visible size/age setting and a "Clear history" action, because this is new
  persistent history (the bridge holds connection rows in memory only today).
  Open to check in phase 2: where the system bridge's state directory is, who owns
  it, and who can connect to the socket.

## Open questions for the owner
1. Who may enable history and request the `none` level: every member of
   `snitchwatch-ui`, or uid 0 only (see the access model)?
2. Default level is `summary-only`; is that too coarse for your first analyses,
   and should `hosts-hashed` be the default instead?

## Phases
1. Plan approved + open questions answered.
2. Add structured fields to the log lines analysis needs (small, bridge-only; may
   land earlier, in r16, as a diagnosability piece).
3. Deterministic export + schema + golden tests + redaction tests.
4. security-reviewer + code-reviewer; docs.
5. Optional offline analysis script on the export.

## Roadmap (not v1)
- Scheduled auto-export to a user-chosen folder.
- Auto-analyze: run the deterministic summary on a schedule and surface findings
  in the GUI; any model-based pass stays opt-in and off-machine only by the
  owner's explicit choice.

## Timing
r17 (r16 is eBPF). Nothing here touches packaging, units, sockets or daemon
config, except enabling history on the system bridge, which will need the tower's
cut process for any state-directory or unit change.
