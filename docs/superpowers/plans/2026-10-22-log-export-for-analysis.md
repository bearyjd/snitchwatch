# Log export for deterministic and probabilistic analysis (DRAFT, for r16)

Status: draft plan, no code. Not for r15. Needs the owner's answers to the open
questions below before any implementation, and a security-reviewer pass because
it moves connection metadata off the bridge.

## Goal
Let the owner (or a tool) analyse what the firewall did, to find issues and
patterns: why unmatched prompts keep arriving, which programs are noisy, what a
new rule would cover, whether feed lag correlates with traffic bursts.

## Where we are today
- Bridge logs with `tracing_subscriber::fmt()` at `info` by default
  (`snitchwatch-bridge-cli/src/main.rs:87`, `EnvFilter`, `RUST_LOG=info` in
  `packaging/systemd/snitchwatch-bridge.service:40`). Output is free text in the
  journal. Lines carry few fields: the "nobody answered" line only had `row_id`
  until the context change on `feat/unanswered-prompt-context` (path +
  destination; no uid, because `ConnectionRow` has none).
- The connection cache already holds structured rows (`ConnectionRow`:
  process, process_path, dst_host/ip/port, protocol, action, matched_rule,
  auto_answer, timings). That is a better analysis source than parsing logs.
- There is a diagnostics report path (`DiagnosticsReport`) for health checks.

## Two layers, built in this order

### 1. Deterministic export (build first)
A bridge-side, read-only export of recent connection rows and events as
newline-delimited JSON, plus a small summary: counts by process, destination,
verdict, matched rule, auto-answer reason; unmatched-prompt rate per minute;
repeat intervals per (process, destination); feed-lag and re-broadcast counts.
- Stable schema with a `schema` version field. Same input gives the same output
  (sorted keys, fixed timestamp format) so it can be tested with golden files.
- Source: the connection cache and counters, not log scraping. Logs stay for
  humans; add structured fields to the few log lines analysis needs.
- Delivery: a CLI subcommand or WS action that writes to a file the user names
  (owner-only mode, via `private_files::write_private`), never a network call.
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
The data names programs, hosts, IPs and local usernames. Rules for the export:
- Off by default; explicit user action per export. No background upload.
- Redaction levels: `none` (local only), `hosts-hashed`, `summary-only`. The
  default for anything that may be shared is the summary without hostnames or
  home-directory paths.
- Strip or hash: usernames in paths (`/home/<user>`), process args/env/cwd (not
  exported at all), private IPs on request.
- File written with `write_private` (0600, ours, no symlink follow), as for
  settings and state files.
- security-reviewer pass on the export path and the redaction (filesystem
  write, user input in paths, data leaving the daemon's trust boundary).

## Decided (owner, 2026-10-22)
- v1 is a manual export: the owner triggers it and gets a file, then drops it
  wherever they want (to an AI by hand, a ticket, a share). No background export,
  no upload, no bridge-to-model call in v1.
- Auto-export and auto-analyze are roadmap items for later, not v1. They need
  their own plan: a schedule, a destination, retention and a stronger privacy
  review, because the data would then leave the machine without a per-export
  decision.

- Redaction is ON by default. A full (unredacted) export is possible only after
  the user explicitly accepts a warning for that export (per export, not a
  saved preference). The warning names what a full export contains: process
  paths, hostnames, IPs and local usernames. The exported file records which
  level it was made at (`redaction` field), so a reader knows what it holds.

- Where it lives: a Kirigami "Export" action is the main entry. The GUI asks
  the running bridge over the authenticated socket (a new request); the bridge
  redacts and returns the data; the GUI writes the file with the user's own
  permissions, in a folder chosen in the save dialog. The system bridge never
  writes to a user-supplied path. No `snitchwatch-bridge-cli` subcommand in v1
  (it would start a second bridge and replace the live socket/token, issue
  #34); a CLI form can later be a thin client of the same request.
- Retention: a rolling window on disk, bounded by size AND age (whichever hits
  first; starting values to tune after measuring: ~32 MiB, ~14 days), as
  rotating append-only segment files, owner-only (0600 file in a 0700 dir) via
  the private-file helpers. Raw rows are stored so a full export is possible;
  redaction happens at export time. Needs: a visible size/age setting and a
  "Clear history" action, because this is new persistent history (the bridge
  holds connection rows in memory only today). Open to check in phase 2: where
  the system bridge's state directory is and who owns it.

## Open questions for the owner
1. (answered above)
2. (answered above: redacted by default; full only after accepting a warning)
3. (answered above: GUI export action, GUI writes the file)
4. (answered above: rolling window on disk, size and age bounded)
Superseded wording kept for the record:
3. Where should it live: bridge CLI subcommand, or a Kirigami "Export" action?
   (Manual trigger suggests an Export button in the GUI plus a CLI form.)
4. Retention: only what the connection cache still holds (capacity-bound), or
   persist a rolling window on disk?

## Phases
1. Plan approved + questions answered.
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
r16 at the earliest (after r15-candidate is pushed). Nothing here touches
packaging, units, sockets or daemon config.
