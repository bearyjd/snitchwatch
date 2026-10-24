# AI-ready cache summary export

Status: **APPROVED by owner 2026-10-24 — implementation and verification pending.**
Baseline: main `d543cfd5765ac62ef631d6542c4062db72e75053` (2026-10-24).
Owner authorized implementation using TDD → PR → independent adversarial
review/fix/re-review → exact-head green CI → merge. Independent security design
review was conditional: implementation must verify the stated privacy, resource,
session and private-save guarantees. Host deployment and persistent history remain
outside this authorization.

## Approved scope and decisions

Owner approved together on 2026-10-24:
1. Deliver a manual cache-only summary phase before persistent history. This changes
   history-first ordering, retaining the
   [export plan](../plans/2026-10-22-log-export-for-analysis.md)'s privacy direction.
2. Accept summary usefulness and disclosure: counts/times/ports, allowlisted-path
   basenames, pseudonymous applications/destinations; no readable destinations or
   persistent pseudonyms. Basenames can identify software and contain untrusted text.
3. Accept one JSON containing schema, dictionary and prompt, with exact preview and
   an explicit save into a dedicated application-private export directory.

Deferred: persistent history/retention, raw rows, hosts-hashed/none modes, JSONL,
CSV, provider APIs/credentials/uploads, local-model runners, schedules, baseline
comparison and rule application. Multi-user history/UID authorization stays open;
this phase adds no stored raw history and no per-user row scoping.

## Existing evidence and boundary

`ConnectionCache` retains recent rows in memory (default capacity 10,000), evicting
oldest nonpending rows; pending rows can exceed capacity. It has no coverage ledger.
Prompt and decided Ping rows are observations, not measured network sessions.
Byte counters are placeholders; direction is hardcoded outgoing. Prompt
timestamps use bridge construction time; decided rows use daemon event time.
GUI filters/collapse/GeoIP state differ. Rebroadcast repairs GUI state, not coverage.

Authenticated clients already read live rows; export adds no UID restriction.
Socket permissions plus the WS handshake remain the access boundary. Basename/redaction consent is disclosure control, not client isolation.

## Request, snapshot and immutable artifact

Add a versioned summary request and correlated success/refusal messages. Stamp the
request with its connection-private `ReplyTo`; never broadcast an artifact or fall
back to broadcast. IDs follow existing 1–64 ASCII alphanumeric/hyphen validation.
Requests are capped at 8 KiB, app selectors at 4 KiB, verdicts at five unique enums;
timestamps must be in `0..=253402300799999` with `from < to`. Use static refusals.
Reject unknown fields/options/filters; no configurable redaction.
Requires updated bridge + GUI; no automatic host deployment. Advertise capability
compatibly; older bridges show export unavailable, never empty success. Choose the
exact handshake hook during implementation planning.

Independent exact filters: optional UTC millisecond window `[from, to)`, exact app
identity (process-path string, or missing-path category), and explicit verdict set
(`allow`, `deny`, `blocklist`, `pending`, `unknown`). No substring/domain/GeoIP query
or GUI-query equivalence. Display export's own filters, a clear scope label and
preview counts; never imply current search/map scope. Filter before grouping. Keep
selectors visible in filter controls; exclude them from artifacts/artifact previews,
errors and logs.

Snapshot under the cache mutex; release before filtering/HMAC/grouping/serialization.
Freeze capture time/selection. Preview/save use identical returned UTF-8 bytes,
never a second request or local rebuild.
The GUI retains one artifact. New requests/cancellation/timeout/session changes
drop it; stale replies cannot reopen preview or enable saving.

Use session-labelled queued Qt delivery, matching
`RulesIoController` guards. One request per session, at most two global workers,
bounded admission with immediate busy refusal. Jobs have a 10-second deadline;
cancellation/disconnect invalidates results and signals cooperative stop. Retain
worker/global admission until exit; never detach running work to admit replacements.
Bound each session to one pending artifact reply; discard unsent replies after five
seconds. Cap serialized WS replies at 1 MiB plus 4 KiB framing/envelope allowance.

## Schema and redaction contract

Allowlisted DTO; never serialize `ConnectionRow`, policy or WS state.
Envelope: format/schema/redaction versions, capture timestamp, UTC semantics,
requested time window, filter-kind flags, source/selection/output counts, coverage,
unavailable fields, human summary, groups, static dictionary and fixed analysis prompt.
Coverage distinguishes examined timestamp range from requested
window; absence of observations never proves absence of traffic.

Groups: app reference/optional disclosed basename, destination reference, port,
allowlisted protocol, verdict, rule class and auto-answer reason; count and observed
first/last timestamps. Include stable within-file group IDs for citations. Unknown
source strings map to fixed `unknown` enums; direction, durations and byte totals are
unavailable. No raw rows/IDs, paths, hostnames, IPs, rule names/text, args, environment,
UIDs, notices or raw diagnostic strings. Exhaustively destructure/classify every
source-row field without `..`, so new fields require review. Rule classes use
explicit default-decision flags and known prefixes; absence alone is not default.

Derive a disclosed basename from a validated path, never `row.process`. Apply the
existing lexical normalization/allowlist (`/usr/`, `/opt/`, `/app/`, `/bin/`, `/sbin/`;
reject `..`, repeated separators and deleted suffix). Full paths always stay out.
Allowlisting proves neither ownership nor package provenance. Otherwise pseudonymize
app identity, including missing-path cases without exposing the raw process label.

Generate a cryptographically random key per export; discard it, never persist/export
it. Use HMAC with distinct app/destination input domains and unambiguous encoding;
retain 128-bit references. Group by canonical identity/full digest; detect shortened
label collisions and regenerate the export/key at most twice, then fail closed.
Fresh pseudonyms prevent hash-based linking; basenames/times/counts can still correlate.
Define destination identity consistently (hostname when present, otherwise IP),
including the translator's host-as-IP fallback. Deterministic tests inject fixed key,
snapshot and time; production exports intentionally use different references.

The provider-neutral prompt treats metadata as untrusted, requests group
citations, separates evidence/speculation, explains incomplete coverage and absent
baselines, and makes advisory recommendations only. No instructions interpolated
from observations. Preview renders plain text, never rich text/HTML/Markdown.

## Resource and coverage limits

Proposed bounds: inspect newest 10,000 rows by insertion order; copy at most
8 MiB of required input strings; omit a row with any required field above 4 KiB;
emit at most 2,000 groups and a 1 MiB UTF-8 artifact including envelope/prompt.
Check lengths before copying/hashing, budget only required inputs, and never scan
beyond the inspection bound for exact omission counts. Use checked arithmetic.
Order rows/groups deterministically; reserve envelope space
and omit whole groups until serialization fits. No partial JSON or silent dropping.
Implementation may tune bounds only with tests/measurement and approval of changes.

Record retained count, inspected count, accepted snapshot count, selected count,
emitted count and separate row/group omission reasons. Pending overflow and each
bounded stage must remain visible; never claim exact full-cache selection counts
when rows were not examined. Evictions, restarts, upstream gaps and missed events
are unknown unless separately measured. State cache-only/incomplete collection.
Avoid unsupported throughput, unique-session, repeat-rate or duration claims.

## Private save

After explicit Save, create a generated non-overwriting `.json` in a dedicated
application-private exports directory (0700, files 0600). Reject unsafe/symlinked
directories; never chmod an arbitrary existing directory. Use exclusive no-follow
creation and atomic no-replace publication; clean temporary files on failure.
Logs/diagnostics/errors contain only static statuses and aggregate sizes: no request,
filter, artifact body, key, basename, destination or save path. Show a static error on
failure and the saved location locally on success. Existing exports
remain until the user removes them; cancellation creates no file. GUI writes bytes;
the bridge accepts no paths/uploads/policy mutations.

## Implementation seams and acceptance

New pure bridge `analysis_export` DTO/filter/redaction module; additive wire/decoder
and private-reply stamping; bounded bridge-CLI worker/routing. New Kirigami controller,
Qt-free view/save helper and QML preview/save, reusing session/feed patterns.
Preserve `rules/io.rs`/`private_files.rs` checks when reusing primitives.

Acceptance: fixture-selected exact filters/counts; exhaustive row-field classification;
hostile path/host/rule/Unicode/control text privacy fixtures; fixed-context golden
output; HMAC domain separation/key freshness/collision handling; enum fallback;
redaction canaries across logs/diagnostics/errors as well as artifact/preview;
every resource/omission boundary; malformed/unsupported request rejection; private
two-client replies; backpressure/cancel/timeout/stale-session behavior; identical
preview/save bytes; safe directory/file modes, symlink and no-overwrite tests.
Use MockOpensnitchd, never a live daemon. Independent security/code review and GUI
interaction evidence precede completion.

Design provenance: local/unpushed; owner approved 2026-10-24. Execute TDD → PR →
independent review/fix/re-review → exact-head green CI → merge. Host deployment
remains separate; conditional design review is not implementation approval.
