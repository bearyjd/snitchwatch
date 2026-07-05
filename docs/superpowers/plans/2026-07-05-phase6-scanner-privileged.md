# Phase 6 — Component B privileged-tier scanner (SCANNER portion)

**Date:** 2026-07-05
**Branch:** `feat/component-b-privileged`
**Spec:** `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`
(baseline model: `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`)

## Scope

This pass implements **only the scanner** portion of Phase 6. The Kirigami
**report UI is explicitly out of scope** — it blocks on Phase 3b's shell
landing first (spec + `IMPLEMENTATION_PROMPT.md` Phase 6), and Phase 3b is
still in progress. No UI work started.

## What landed

New workspace member `crates/scanner-privileged/` — a **separate, on-demand
binary** (`snitchwatch-scanner-privileged`) invoked via polkit/pkexec. It is
NOT a daemon: no listener, socket, timer, background thread, or systemd unit
anywhere in the crate. Every host read is a one-shot; the process runs one
scan and exits. Protecting this property is the whole reason Component B's
privileged tier exists.

Ordered sub-checks in one polkit-gated invocation (spec §3):

1. **file_drift** — deferred: owned by the shared baseline cache
   (`baseline.db`), which is Phase 5's sibling-branch work. Recorded as a
   `SkippedCheck` so the integration point is explicit, not silently dropped.
2. **kargs_drift** — `/proc/cmdline` vs `rpm-ostree kargs` committed set, with
   a curated bootloader/kernel-injected allowlist (`BOOT_IMAGE=`, `root=`,
   `ostree=`, `initrd=`, `rd.*`). Same checksum-first / dynamic-allowlist /
   else-anomalous shape as the baseline file rule.
3. **module_anomaly** — each `/proc/modules` entry classified through the
   baseline's three-tier provenance model (base tree → layered package →
   anomalous), signature status informing severity. No hardware-dependent
   static module allowlist.
4. **lockdown_state** — Secure Boot (`mokutil --sb-state`) + kernel lockdown
   (`/sys/kernel/security/lockdown`). Flags on *weakening transition* (via a
   `state_snapshots` table), not absolute state.
5. **rootkit_signature** — wraps **`chkrootkit`** (not `rkhunter`), run last
   (slowest). Presence-checked; absent → skipped with note, never "clean".

All findings feed one `scans.db` schema (`scan_runs` + `findings` with the
`check_type` discriminator column) matching the baseline spec §4, with the
new/still-outstanding/resolved reconciliation diff implemented in the store.

`packaging/polkit/org.snitchwatch.scanner.policy` — single action
`org.snitchwatch.scanner.run-deep-scan`, `auth_admin_keep`.

## Testability seam

All host reads go through the `SystemFacts` trait. Classification/diff logic
is unit-tested with synthetic `/proc/cmdline`, `rpm-ostree` output, module
lists, and chkrootkit text — no real hardware needed. `LiveSystem` is the
production shell-out impl.

## Phase 5 reconciliation (REQUIRED at merge)

Phase 5's userspace crate will define its own `scans.db` store against the
same spec. When both branches merge, the two stores MUST be unified into one
shared schema/crate (obvious home: a `scanner-store` crate). This crate's
store is kept deliberately schema-compatible so that merge is lift-and-share,
not a rewrite. See the module docs in `src/store.rs`.

## Verified vs. not verifiable here

- **Verified in sandbox:** `cargo check`/`test`(40)/`clippy -D warnings`
  clean; binary runs end-to-end and degrades gracefully; real
  `/sys/kernel/security/lockdown` read works (`none`).
- **Not verifiable here (needs real hardware / tools):** actual `chkrootkit`
  output parsing against a live scan (not installed); `rpm-ostree kargs` /
  `rpm -V` / `modinfo` provenance on a real Bazzite/Silverblue host;
  `mokutil --sb-state` (not installed); polkit prompt/deny flow (no polkit
  daemon in sandbox); chkrootkit false-positive spot-check on OSTree layout
  (spec §1 flagged this as needing a real check).
