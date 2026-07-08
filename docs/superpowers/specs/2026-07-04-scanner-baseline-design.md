# Bazzite Security Scanner (Component B) — atomic-baseline design spec

**Date:** 2026-07-04
**Status:** Draft, awaiting user review
**Audience:** Implementer (you, future-you, contributors) — this is Phase 4
of `IMPLEMENTATION_PROMPT.md`, feeding directly into Phase 5 (userspace tier
implementation). It is a **design deliverable, not code** — no Component B
crate exists yet and none should be started before this doc is approved.

## Summary

Component B's hardest problem, per `AUDIT.md`'s "Deferred: Component B
baseline design" section, is telling apart **expected** filesystem state on
an rpm-ostree atomic system from **genuinely anomalous** state, without
either drowning the user in false positives after every OS update/layer
change or missing real tampering by trusting too broadly. This spec answers
the four deferred questions with concrete, implementable rules, evaluates
the two candidate approaches named in `AUDIT.md`, and recommends one.

The core move: **don't reimplement what `ostree`/`rpm-ostree`/`rpm` already
know.** Component B's own code is a thin orchestration + diffing/reporting
layer over three existing sources of truth (the OSTree content-addressed
object store, `rpm-ostree status`/`db diff`, and `rpm -V`/`rpm --verify`),
not a hand-rolled manifest-checksum engine.

## Goals

- A classification rule that any file-level finding can be run through to
  get `expected` or `anomalous`, with no ambiguous middle category.
- A baseline strategy that matches the userspace-tier-frequent /
  privileged-tier-rare split from the original brief.
- A concrete answer for how layered-package trust interacts with
  integrity checking, that still catches a compromised layered package.
- A stored-state schema that makes "what changed since last scan" an
  actual diff, not a full re-dump every run.

## Non-goals

- Judging whether a layered package's *own shipped content* is malicious
  (supply-chain trust). This is a drift/tamper detector, not a package
  reputation system — if a file matches exactly what the RPM shipped, it's
  in scope for a different tool, not this one.
- Deciding the actual scan trigger/scheduling UX, retention policy, or
  notification behavior — flagged as stop-and-asks below; these are
  product calls, not baseline-design technical ones.
- Rootkit/kernel-level detection mechanics — out of scope for this spec,
  which is about filesystem drift classification specifically. (Rootkit
  scanning was named in `AUDIT.md`'s Phase 6 privileged-tier scope; it's a
  separate detection mechanism from the drift baseline this doc covers.)

## 1. Classification rule: expected drift vs. anomaly

Every changed/flagged path is run through this decision tree, in order.
The first matching rule decides the classification — there is no
ambiguous outcome.

1. **Out of scan scope.** Path is under an inherently mutable location
   never scanned for drift at all: `/var/log`, `/home`, `/tmp`, `/run`
   (transient by definition), user data directories. → **not applicable**
   (not a finding either way).
2. **Base tree, checksum matches.** Path is owned by the active
   deployment's base OSTree commit (per `rpm-ostree status --json`'s
   `checksum` field) and its current content matches the object the
   commit records for that path (OSTree is content-addressed — this is a
   direct object-store lookup, e.g. via `ostree fsck` or comparing the
   file's checksum against the commit's tree, not a hand-computed hash
   comparison). → **expected**.
3. **Layered package, checksum matches its own manifest.** Path is owned
   by a package in the deployment's layered-packages list (per
   `rpm-ostree status --json`'s `packages`/`requested-packages`) and its
   current content/mode/owner matches that RPM's own recorded metadata
   (`rpm -V <pkg>` reports no diff for this path). → **expected**.
4. **Known dynamic/generated location.** Path falls under a curated
   allowlist of paths that are *supposed* to be written post-install and
   diverge from any static manifest by design: `systemd-tmpfiles.d`- and
   `sysusers.d`-declared paths, systemd-generated runtime units, and the
   `/etc` files OSTree's own 3-way merge already tracks as
   user-modifiable-since-default (surfaced via `ostree admin
   config-diff`, which already distinguishes "default `/usr/etc` value"
   from "locally modified `/etc` value" — reuse that distinction rather
   than re-deriving it). → **expected**, regardless of checksum, provided
   the path is on this allowlist.
5. **Everything else.** A path inside a nominally-immutable location
   (`/usr`, or a layered package's claimed files) whose content does not
   match the base tree's commit, does not match its owning layered
   package's shipped checksum, and isn't on the dynamic-path allowlist —
   including files with no owner at all sitting inside `/usr`. →
   **anomalous**.

This gives an implementable, checksum-first algorithm with three known-good
reference sources (base commit, layered-RPM manifests, curated dynamic
allowlist) and a strict fallthrough to anomalous — no step requires
inventing a new manifest format.

## 2. Baseline computation: fresh vs. cached

**Decision: split by cost, not by an all-or-nothing cache policy.**

- **Cheap part — recompute fresh, every scan, any tier.** "What packages
  and what base commit are currently active" comes straight from
  `rpm-ostree status --json` — this is already fast (it's reading local
  state rpm-ostree maintains, not re-deriving anything) and changes
  exactly when a deployment changes. There's no reason to cache this; the
  read cost is negligible and caching it risks exactly the staleness bug
  this question is trying to avoid (booting a new deployment, or adding/
  removing a layered package, must be reflected immediately, not on the
  next cache-invalidation cycle).
- **Expensive part — cache, keyed by deployment checksum, invalidate on
  change.** Full-content integrity hashing (the AIDE-style pass: hashing
  every file under `/usr` and every layered package's claimed files) is
  the genuinely expensive operation. Cache the computed baseline at
  `/var/lib/snitchwatch-scanner/baseline-<deployment-checksum>.db`,
  computed once per unique deployment checksum the system boots into.
  Invalidate (recompute) only when `rpm-ostree status --json`'s active
  deployment checksum changes — not on a timer, not on every scan.

**Justification against realistic scan frequency (the userspace/privileged
split from the brief):** the userspace tier is meant to run often and
cheaply with no privilege escalation; it should lean entirely on the
*cheap* metadata (package/deployment state) plus lightweight per-file
signals (mtime/size against the cached baseline's recorded values) and
should **not** trigger a full content-hash pass itself. The privileged
tier is invoked rarely, on-demand, specifically because full AIDE-style
hashing is the expensive operation that justifies asking for elevated
access in the first place — that's exactly the tier that should own
building/refreshing the cached hash baseline, and it only needs to
actually do that work when the deployment checksum has changed since the
last cached baseline. Net effect: most scans (frequent, userspace) are
cheap metadata/mtime checks against an already-cached baseline; the rare,
privileged, expensive full-hash pass only re-runs when the OS itself
changed underneath it.

## 3. Layered-package allowlisting vs. AIDE-style integrity

**Decision: per-file checksum matching against the RPM's own shipped
manifest — never whole-package blanket trust.**

A layered package being present and "allowlisted" only means: *this
package's own files, exactly as it shipped them, are trusted.* It does
**not** mean every file under paths the package touches is automatically
trusted going forward. Concretely:

- For every file a layered RPM claims to own, verify it against that
  RPM's own recorded checksum/mode/owner — `rpm -V <pkg>` already does
  exactly this comparison; reuse it rather than reimplementing a
  manifest-checksum database.
- If a layered package's file matches its own shipped manifest →
  **expected**, full stop — evaluating whether the package's own shipped
  content is itself malicious is a supply-chain/reputation problem, not a
  drift-detection problem (see Non-goals).
- If a layered package's file does **not** match its own shipped manifest
  → **anomalous**, regardless of the package's presence on any allowlist.
  This is precisely the threat model requirement from `AUDIT.md`: a
  compromised or malicious layered package tampering with its own files
  post-install (or a post-install scriptlet writing files outside its own
  manifest) must still be caught — "the package is allowlisted" can never
  become "anything touching a path near this package is trusted."
- A file with no owning package at all, sitting inside a nominally
  immutable path, is anomalous by rule 5 above regardless of any
  allowlist — allowlisting is additive trust for specific claimed files,
  never a blanket exemption for a directory or package name.

## 4. Stored prior-scan state — schema for real diffing

SQLite, following the existing `rusqlite` convention already used in this
repo for Component A's blocklist store
(`crates/snitchwatch-bridge/src/blocklists/store.rs`). Two logical stores,
because they have different write-privilege owners:

**Baseline cache** (written by the privileged tier only, read by both):
`/var/lib/snitchwatch-scanner/baseline.db`

```sql
CREATE TABLE deployments (
    deployment_checksum TEXT PRIMARY KEY,
    base_checksum        TEXT NOT NULL,
    layered_packages_json TEXT NOT NULL, -- rpm-ostree status packages[] snapshot
    computed_at          TEXT NOT NULL   -- ISO8601, when this baseline was built
);

CREATE TABLE baseline_entries (
    deployment_checksum TEXT NOT NULL REFERENCES deployments(deployment_checksum),
    path                 TEXT NOT NULL,
    expected_source      TEXT NOT NULL,  -- 'base_tree' | 'layered_pkg:<name>' | 'dynamic_allowlist'
    expected_checksum    TEXT,           -- NULL for dynamic_allowlist entries (checksum irrelevant there)
    expected_mode        INTEGER,
    expected_owner       TEXT,
    PRIMARY KEY (deployment_checksum, path)
);
```

**Scan history / findings** (owned by the userspace tier, holds the actual
"what changed" state): `$XDG_STATE_HOME/snitchwatch-scanner/scans.db` (or
system-level equivalent if the scheduling stop-and-ask below resolves
toward a system service rather than a per-user one)

```sql
CREATE TABLE scan_runs (
    scan_id              INTEGER PRIMARY KEY,
    started_at           TEXT NOT NULL,
    finished_at          TEXT,
    deployment_checksum  TEXT NOT NULL,
    tier                 TEXT NOT NULL   -- 'userspace' | 'privileged'
);

CREATE TABLE findings (
    finding_id           INTEGER PRIMARY KEY,
    path                 TEXT NOT NULL,
    classification       TEXT NOT NULL,  -- 'anomalous' (only anomalies are rows here; 'expected' isn't stored)
    detail               TEXT,           -- e.g. "checksum mismatch vs layered_pkg:firefox"
    first_seen_scan_id   INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    last_seen_scan_id    INTEGER NOT NULL REFERENCES scan_runs(scan_id),
    resolved_at_scan_id  INTEGER REFERENCES scan_runs(scan_id) -- NULL while still outstanding
);
```

**Diff logic for "what changed since last scan":** on each new scan run,
compute the anomaly set, then reconcile against the most recent prior
scan's still-open (`resolved_at_scan_id IS NULL`) findings:

- Anomaly present now, not in prior open set → new row, `first_seen_scan_id
  = last_seen_scan_id = this scan`. Reported as **new**.
- Anomaly present now, also in prior open set → update `last_seen_scan_id`.
  Reported as **still outstanding**.
- Prior open finding, not present now (back to `expected`) → stamp
  `resolved_at_scan_id = this scan`. Reported as **resolved since last
  scan**.

The report shown to the user is these three buckets, not a flat re-dump of
every anomaly on every run.

## Candidate approach evaluation

`AUDIT.md` named two options: (a) build a signed-commit-diff + allowlist +
usroverlay-exclusion engine in Component B itself, or (b) delegate as much
as possible to `rpm-ostree db diff` and existing primitives.

**Recommendation: (b), with the file-level integrity check delegated to
`rpm -V` and the base-tree check delegated to OSTree's own object store —
Component B owns orchestration and the findings/diff database, not any of
the underlying manifest logic.**

Reasoning: each layer of the classification rule above already has an
existing, correct, actively-maintained primitive that answers it —
`rpm-ostree status --json` / `rpm-ostree db diff` for package-set-level
questions ("what's layered, what changed between deployments"), `rpm -V`
for per-package file-integrity questions (this already reads each RPM's
own manifest checksums — reimplementing this as a hand-rolled
manifest-checksum database, which is what pure option (a) implies, means
maintaining a second, divergence-prone copy of information `rpm` already
tracks correctly), and OSTree's content-addressed object store for
base-tree integrity (verifying a file against a signed commit is a direct
object lookup, not something to reimplement as a diff engine).
`rpm-ostree db diff` alone is *not* sufficient by itself, though — it
answers package-set drift, not "has an installed file's bytes been
tampered with since install," which is why the file-level `rpm -V` /
OSTree-object-lookup layer has to sit on top of it. So this isn't a pure
pick of (b) over (a): it's (b)'s philosophy — delegate to existing
primitives at every layer where one exists — applied specifically to
*both* the package-set layer and the file-integrity layer, rather than (a)'s
implied approach of Component B computing and owning a from-scratch
manifest/checksum system end to end.

## Stop-and-ask — product/risk-tolerance calls, not resolved here

These are genuine judgment calls for the human owner, not technical
baseline-design questions, and are deliberately left open rather than
guessed:

1. **Userspace-tier scheduling.** Does it run on a systemd `--user` timer
   (daily/hourly), only on login, or purely on-demand via a "Scan now"
   button? This affects resource/battery impact and UX expectations and
   should be an explicit product decision before Phase 5 wires up any
   scheduling.
2. **Baseline-cache ownership split.** This spec assumes the privileged
   tier builds/refreshes the cached full-hash baseline (since it's the
   expensive operation justifying elevated invocation), but many files
   under `/usr` are world-readable and hashable without privilege — should
   the userspace tier be allowed to build its own baseline cache
   opportunistically when it can, falling back to "needs privileged scan"
   only for genuinely privilege-gated paths? This changes the concrete
   division of work between the two tiers and is worth an explicit call.
3. **Resolved-finding handling.** Should a finding that reverts to
   `expected` on a later scan auto-clear (per the diff logic in §4), or
   does a security-focused tool want to require explicit user
   acknowledgment ("mark investigated") before a finding is considered
   closed? Auto-clearing risks a "reverted after being noticed, no one
   ever saw the report" gap; requiring acknowledgment adds friction. This
   is a risk-tolerance call, not a technical one.
4. **Notification behavior on new anomalies.** Should Component B
   proactively notify (mirroring Component A's tray-notification pattern
   in `crates/snitchwatch-tauri/`) the moment a new anomaly is found, or
   stay silent until the user opens the report UI? Ties into the shared
   design-system decision from `AUDIT.md` decision #1 and should be
   decided alongside Phase 6's report UI, not assumed here.

## Decision log

| Decision | Alternatives | Why this won |
| --- | --- | --- |
| Checksum-first classification tree with strict fallthrough to anomalous | Allowlist-only classification (path-based, no checksum verification) | Path-only allowlisting can't catch tampering of an otherwise-trusted file; checksum-first still catches a compromised file inside an "expected" path |
| Split baseline cost: cheap metadata fresh every scan, expensive hash cache invalidated on deployment-checksum change | Cache everything on a timer; recompute everything every scan | Matches the userspace-frequent / privileged-rare split from the brief without either staleness risk or wasted CPU on every cheap scan |
| Per-file checksum trust for layered packages, never whole-package trust | Trust any file under a path a layered package "owns" | Required by the stated threat model — a compromised layered package tampering with its own files after install must still be caught |
| Two-store schema: `baseline.db` (privileged-owned cache) + `scans.db` (userspace-owned history/findings) | Single combined store | Different write-privilege owners; combining them either over-privileges the userspace tier's store or forces every scan-history write through the privileged tier |
| Delegate to `rpm-ostree status/db diff`, `rpm -V`, and OSTree's object store rather than building a from-scratch manifest engine | Build Component B's own signed-commit-diff + manifest-checksum system end to end | Reimplementing what `rpm`/`rpm-ostree`/`ostree` already track correctly risks silent divergence from upstream's own definition of package/commit state |
