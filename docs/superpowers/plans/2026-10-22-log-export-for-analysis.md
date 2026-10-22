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
JSON, plus a small summary: counts by process, destination, verdict, matched
rule, auto-answer reason; unmatched-prompt rate per minute; repeat intervals per
(process, destination); feed-lag and re-broadcast counts.
- Stable schema with a `schema` version field. Same input gives the same output
  (sorted keys, fixed timestamp format) so it can be tested with golden files.
- Source: the connection cache and counters, not log scraping. Logs stay for
  humans; add structured fields to the few log lines analysis needs.
- Delivery: ONE path. The GUI sends a new WS request over the authenticated
  socket; the bridge builds and redacts the export and returns it; the GUI writes
  the file with the user's own permissions (GUI side: `private_files::write_private`
  in `snitchwatch-kirigami`). The bridge never writes to a user-named path and
  never makes a network call. There is no CLI subcommand in v1.
- The bridge's own on-disk history (below) uses the bridge crate's private-file
  helper (`snitchwatch-bridge/src/state_file.rs`), not the GUI's.
- Tests: golden-file test with fixture rows (use `MockOpensnitchd` per
  CLAUDE.md, never a live daemon); redaction tests (below).

### 2. Probabilistic analysis (separate, later)
Pattern clustering and "what is probably happening" guesses over the
deterministic export. Runs OUTSIDE the bridge, on the exported file:
- an offline script that uses only the summary (counts, intervals), or
- an LLM pass the owner starts by hand on an export they have reviewed.
The bridge never calls out to a model. Probabilistic output is advisory and
labelled as such; it never edits rules.

## Privacy and security (why this needs review)
The data names programs, hosts, IPs and local usernames.
- Off by default; explicit user action per export. No background upload.
- Redaction levels, each defined by what it CONTAINS:
  * `summary-only` (default): aggregate counts and intervals only, keyed by
    process basename and a keyed hash of the destination. No raw rows, hostnames,
    IPs or paths.
  * `hosts-hashed`: raw rows with hostnames and IPs replaced by a keyed hash;
    paths scrubbed (below); ports, protocol, verdict and process basename kept.
  * `none`: raw rows. Only after the user accepts a per-export warning that names
    what it contains (process paths, hostnames, IPs, local usernames).
- Hashing: HMAC with a random per-export key that is generated for the export and
  DISCARDED (never written to the file), so a reader cannot dictionary-test common
  hostnames against the hashes, and two exports cannot be linked by hash. Values
  are stable within one export.
- Path scrub: replace the username in `/var/home/<user>` (Bazzite/atomic),
  `/home/<user>`, `/root`, `/var/roothome` and `.var/app/<app-id>` paths with
  `<user>`; process args, env and cwd are never exported. A fixture with each
  path form must pass.
- The exported file records its level (`redaction` field).
- Access model (honest limits): the per-export warning is a consent control, not
  a security boundary, because any authenticated socket client can already read
  every live row. The real boundary is who can connect to the bridge socket; the
  system socket is root-only after #35's work, to be re-verified before history is
  enabled. Per-user scoping on a multi-user host needs a uid on `ConnectionRow`
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
1. Who may trigger an export on a multi-user host (see the access model)?
2. Default level is `summary-only`; is that too coarse for your first analyses,
   and should `hosts-hashed` be the default instead?

## Phases
1. Plan approved + open questions answered.
2. Add structured fields to the log lines analysis needs (small, bridge-only).
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
