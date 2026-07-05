# Phase 5 — Component B userspace tier

**Date:** 2026-07-05
**Status:** Implemented
**Implements:** `IMPLEMENTATION_PROMPT.md` Phase 5, per the approved
`docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`.

## Scope decision — connection-log signal channel deferred

Phase 5's precondition is a stop-and-ask on building connection-log
persistence in Component A (its cache is in-memory only). Per the
implementation prompt's own stated fallback ("If the answer is 'not needed
yet,' Phase 5 proceeds without this and Component B's userspace tier stands
alone for its first release"), we **ship Component B's userspace tier
standalone** and do **not** build connection-log persistence in Component A
in this pass. This is a deliberate scope decision, not an oversight. The
"unusual outbound connections" check is wired in as an explicit
[`CheckOutcome::Deferred`], surfaced in every scan report's
`deferred_checks` bucket, so the gap is visible rather than hidden.

## Crates

Mirrors the `snitchwatch-bridge` / `snitchwatch-bridge-cli` split:

- `crates/scanner-core/` — library: classification tree, provenance,
  stores, checks, orchestration.
- `crates/scanner-cli/` — thin binary printing a JSON scan report.

## Design mapping to the baseline spec

| Baseline design section | Implementation |
| --- | --- |
| §1 five-step classification tree | `classify.rs` — pure `classify(&PathFacts)`, first-match, strict fallthrough to `Anomalous`. |
| §1 reference sources assembled live | `provenance.rs` (`rpm -qf` + `rpm -V` proxy at the userspace tier), `deployment.rs` (`rpm-ostree status --json`), `rpmverify.rs`. |
| §2 cheap-fresh vs expensive-cached | Userspace tier recomputes deployment metadata fresh, never runs the full content-hash pass (that's Phase 6). `baseline.db` cache keyed by deployment checksum is defined (`stores/baseline.rs`) for the privileged tier to populate. |
| §3 per-file layered trust | `LayeredMatch::{Clean,Modified}`; a modified layered file is anomalous regardless of allowlist. |
| §4 two-store schema + diff | `stores/baseline.rs` (`baseline.db`), `stores/scans.rs` (`scans.db`) with the new / still-outstanding / resolved reconcile. |

## Userspace checks (all privilege-free)

- `systemd_user` — new/changed `~/.config/systemd/user` units + autostart
  `.desktop` entries.
- `shell_rc` — modifications to tracked shell rc/profile files.
- `flatpak` — Flatpak permission (override) drift.
- `listeners` — new/changed listeners on user-reachable ports (`ss`).
- `outbound` — **deferred** (see scope decision above).

Non-`/usr` user-writable surfaces use a trust-on-first-use baseline stored
in the userspace-owned `scans.db` (`userspace_baseline` table). This takes
the "yes for non-privileged paths" branch of baseline design stop-and-ask
§2; the privileged `/usr` hash baseline stays in `baseline.db`.

## Testability

All shell-outs sit behind `inspector::SystemInspector`
(`RealSystem` in prod, `testkit::MockInspector` in tests). The hard logic —
the classification tree and the findings diff — is pure and unit-tested with
no rpm-ostree/ostree/flatpak/ss present.
