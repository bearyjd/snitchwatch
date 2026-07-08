# Bazzite Security Scanner (Component B) — privileged-tier design spec

**Date:** 2026-07-04
**Status:** Draft, awaiting user review — specifies `IMPLEMENTATION_PROMPT.md`
Phase 6's currently one-line scope ("AIDE-style integrity check, rootkit
scan, rpm-ostree diff") concretely, before that phase starts implementation.
**Audience:** Whoever implements Phase 6.

## Summary

`docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` already
solved the file-drift/baseline-classification problem (the AIDE-style
integrity check) by delegating to `rpm-ostree status/db diff`, `rpm -V`,
and OSTree's object store, and explicitly scoped rootkit/kernel-module
detection **out** as "a separate detection mechanism from the drift
baseline." This doc picks up that deferred piece: what actual tool/
mechanism does the rootkit scan wrap, what exactly does the kernel-module/
kargs audit check and against what expected-state source, how do these
integrate with the already-scoped integrity check in one privileged-tier
invocation, and what do their findings look like in the existing
`scans.db` schema. Same delegate-don't-reimplement philosophy as the
baseline doc throughout.

## 1. Rootkit/malware signature scanning — recommend `chkrootkit`, not `rkhunter`

**Recommendation: `chkrootkit`.** Research into both tools named in the
original brief:

- **`rkhunter`** — last major stable release was 1.4.6 (February 2018).
  It has received sponsorship-funded bug-fix maintenance since December
  2023 and moved to GitHub, but no major new-detection-capability release
  since 2018. Its core mechanism (a "known-bad" signature list plus a
  file-properties baseline comparison) means its detection set is
  substantially frozen relative to rootkit techniques that have emerged
  since — notably it has no meaningful coverage of eBPF-based rootkits,
  a real and growing category on modern Linux kernels. **Not
  recommended as the primary tool given how stale its detection
  signatures are relative to current threats.**
- **`chkrootkit`** — actively maintained, with releases incorporating
  detection for current, real-world threats (the XZ Utils backdoor,
  the "Bootkitty" UEFI bootkit, memory-executed/fileless process
  detection) landing recently. Structurally, `chkrootkit` leans more on
  behavioral/signature checks (known trojaned-binary signatures, hidden
  process detection via `/proc` cross-checks, promiscuous-NIC detection,
  known LKM rootkit hooks) than on a static "expected file properties"
  database — this also makes it structurally less likely to false-positive
  against OSTree's read-only, hardlinked, immutable-bit file layout than
  a property-baseline-style checker would be, though **no confirmed
  report of either tool's behavior on Bazzite/Silverblue specifically was
  found during this research** — this is a reasoned inference from how
  each tool works, not a verified compatibility test, and should be
  spot-checked in Phase 6's implementation, not assumed clean.
- Both tools are read-only scanners (they inspect `/proc`, `/usr/bin`,
  `/sbin`, etc., and don't need to write anything under `/usr`) — this
  part is fine on an immutable base tree regardless of which is chosen.
  Any tool-internal state (baseline snapshots, logs) writes to `/var`,
  which is writable/stateful on rpm-ostree systems as normal.

**Wrap `chkrootkit` as an external process invocation** (parse its
plain-text output — its output format is stable and line-oriented, not
something to reimplement detection logic for) rather than reimplementing
any of its checks natively. This is the same "delegate, don't reimplement"
posture the baseline doc already established for `rpm -V`/`rpm-ostree`.

## 2. Kernel module/kargs audit — concrete mechanism and expected-state sources

Two distinct sub-checks, each with its own delegated primitive and
expected-state source — no bespoke detection logic invented for either:

### Kernel argument (kargs) drift

- **Mechanism:** compare the currently booted kernel's actual command
  line (`/proc/cmdline`) against the kernel arguments `rpm-ostree kargs`
  reports as committed for the active deployment.
- **Expected-state source:** `rpm-ostree kargs` (no arguments) lists the
  deployment's own recorded kernel argument set — this is the existing
  primitive, not something to re-derive from `rpm-ostree status --json`
  manually.
- **Classification rule:** any argument present in `/proc/cmdline` that is
  neither in the deployment's `rpm-ostree kargs` list nor in a small
  curated allowlist of arguments the bootloader/kernel itself injects at
  boot time regardless of committed kargs (e.g. `BOOT_IMAGE=`,
  `root=`, `ostree=`, initrd-added values) is flagged as **kargs drift** —
  this is the same "checksum-first, curated-dynamic-allowlist,
  else-anomalous" shape as the baseline doc's file-classification rule,
  applied to boot arguments instead of files.

### Loaded kernel module audit

- **Mechanism:** enumerate currently loaded modules (`/proc/modules` or
  `lsmod`), and for each, determine provenance and trust the same way the
  baseline doc classifies files — **do not build a separate static
  "expected modules" allowlist**, since expected modules are inherently
  hardware-dependent (a laptop's Wi-Fi/Bluetooth modules, a desktop's
  GPU driver, etc. vary machine to machine) and a fixed list would be
  both brittle and a maintenance burden distinct from the project's core
  scope.
- **Expected-state source, reusing the baseline doc's own three tiers:**
  1. Is the module shipped under the base OSTree commit's own
     `/usr/lib/modules/$(uname -r)/` tree? → expected (base tree,
     verifiable the same way the baseline doc verifies any other base-tree
     file — an OSTree object-store lookup, not a new mechanism).
  2. Is the module shipped by a layered/DKMS-style package (tracked in
     `rpm-ostree status --json`'s layered packages, e.g. `akmod-nvidia`)?
     → expected, verified via `rpm -V` against that package's own
     manifest, exactly as the baseline doc already does for any other
     layered-package file.
  3. Neither of the above → **anomalous**, with severity informed by
     signature status: check `modinfo -F sig_key <module>` (or equivalent
     signature presence check) — an unrecognized, unsigned module loaded
     outside both the base tree and any known layered package is exactly
     the signal a rootkit-installed kernel module would produce.
- This deliberately reuses the baseline doc's existing three-tier
  provenance model (base tree / layered package / anomalous) rather than
  inventing a fourth, module-specific classification scheme — the same
  "don't reimplement what's already been designed" discipline applies
  within Component B's own prior design, not just toward upstream tools.

### Secure Boot / kernel lockdown state check

- **Mechanism:** read `/sys/kernel/security/lockdown` (current lockdown
  mode: `none`/`integrity`/`confidentiality`) and Secure Boot state via
  `mokutil --sb-state` (or the equivalent EFI variable read).
- **Expected-state source:** a user/deployment-level setting (does this
  system expect Secure Boot enabled and lockdown active?) — **this is a
  product/config question, not something this spec resolves**; flag as a
  stop-and-ask for whoever configures Component B's defaults, analogous to
  the baseline doc's own stop-and-ask list. Absent an explicit expectation,
  the conservative default is: report the current state as informational,
  only escalate to an anomaly finding if the state has *changed* since the
  last scan (Secure Boot was on, is now off) — a state transition is a much
  stronger anomaly signal than an absolute "off" state that might simply
  reflect the user's own hardware/firmware choice.

## 3. Integration with the AIDE-style integrity check — one invocation, ordered sub-checks, shared schema

**Recommendation: all privileged-tier checks run as ordered sub-checks
within a single invocation of the same `crates/scanner-privileged/`
binary, gated by the same polkit action, producing one scan report —
not architecturally separate tools/binaries/policies.**

Rationale: they share the same trust boundary (all require the same
elevated read access — reading arbitrary `/proc` entries, invoking
`rpm -V` against arbitrary packages, reading kernel security state) and
the same invocation trigger (on-demand, polkit-gated, per
`IMPLEMENTATION_PROMPT.md` Phase 6's existing "never a persistent daemon"
constraint). Splitting them into separate polkit actions/binaries would
mean prompting the user for privilege escalation multiple times per
logical "run a deep scan" action, with no corresponding security benefit —
none of these checks needs a narrower privilege scope than the others.

**Ordering within one invocation:**
1. File-integrity/drift check (per the baseline doc — already fully
   specified there).
2. Kargs drift check (cheap, no external process spawn beyond `rpm-ostree
   kargs` and a `/proc/cmdline` read).
3. Loaded-module audit (cheap, `/proc/modules` + per-module `rpm -V`/
   `modinfo` lookups).
4. Secure Boot/lockdown state check (cheap, two file/command reads).
5. `chkrootkit` invocation (the slowest step — a full-system rootkit scan
   — run last so a user who cancels a long-running scan still gets the
   faster, cheaper checks' results recorded).

## 4. Concrete output — extend the existing `findings` schema, don't create a parallel one

The baseline doc's `scans.db` `findings` table (see
`2026-07-04-scanner-baseline-design.md` §4) already has the right shape
for "new / still-outstanding / resolved" diffing — extend it with one
column rather than building a second table:

```sql
ALTER TABLE findings ADD COLUMN check_type TEXT NOT NULL DEFAULT 'file_drift';
-- check_type values: 'file_drift' | 'kargs_drift' | 'module_anomaly'
--                     | 'lockdown_state' | 'rootkit_signature'
```

The existing `path` column is reused as a synthetic identifier for
non-filesystem findings, since the new/still-outstanding/resolved diff
logic (matching on `path` across scans) works identically regardless of
what the string represents:

| `check_type` | Example `path` | Example `detail` |
| --- | --- | --- |
| `file_drift` | `/usr/bin/curl` | (per baseline doc — checksum mismatch vs. `layered_pkg:curl`) |
| `kargs_drift` | `kargs:mitigations=off` | `"runtime kernel arg 'mitigations=off' not present in rpm-ostree kargs for deployment <checksum>"` |
| `module_anomaly` | `module:xyz_hidden` | `"loaded module 'xyz_hidden' not found in base tree /usr/lib/modules nor any layered package; unsigned (modinfo sig_key empty)"` |
| `lockdown_state` | `lockdown:secureboot` | `"Secure Boot state changed: was enabled at last scan, now disabled"` |
| `rootkit_signature` | `rootkit:chkrootkit:suckit` | (chkrootkit's own output line, passed through verbatim — don't reformat/reinterpret its detection text) |

This means Phase 5's userspace tier and Phase 6's privileged tier share
one `scan_runs`/`findings` schema and one diff/report-rendering code path
in the eventual report UI — the UI doesn't need a separate rendering
branch per check type beyond a label/icon keyed on `check_type`.

## Does this change Phase 6's scope from what `IMPLEMENTATION_PROMPT.md` currently assumes?

Mostly confirms and specifies rather than changes: the "single privileged
binary, polkit-gated, no persistent daemon" shape Phase 6 already commits
to is exactly right and unchanged. Two things worth flagging as scope
clarifications, not scope changes:

- **`rkhunter` is explicitly *not* the tool to wrap** — if anyone assumed
  "AIDE-style + rkhunter" from the original brief's naming both `rkhunter`
  and `chkrootkit`, that assumption should be corrected now, before
  implementation starts around the wrong tool.
- **The kernel-module/kargs/lockdown checks are new, concrete acceptance
  criteria for Phase 6** that weren't previously itemized (Phase 6's
  one-liner only said "rootkit scan, rpm-ostree diff") — Phase 6's
  acceptance criteria should be updated to include these three sub-checks
  explicitly, not just "a rootkit scan," so they aren't dropped silently
  during implementation.
