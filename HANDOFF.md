# Linux App Firewall + Bazzite Security Scanner — Handoff (updated 2026-07-12)

> **Read this first if you're picking this repo up cold.** Everything below
> the "Current status" section is the *original* handoff from 2026-07-04,
> kept for history/decision rationale — it is accurate as a record of what
> was decided and why, but stale as a status report. Trust this section for
> "what's true today."

## Current status (2026-07-12)

Both components are **code-complete**. Everything reachable from a sandbox
without real Bazzite hardware has been built, tested, and verified — CI is
green on real GitHub Actions, not just locally. What's left is exclusively
real-hardware verification and one release-timing decision, both described
below.

**Component A (Snitchwatch):**
- Phases 1 (auth/socket), 2 (packaging), 3a/3b (Kirigami shell) are done.
- The shell that ships is `crates/snitchwatch-kirigami/` (Qt6/QML +
  Kirigami via `cxx-qt`) — feature-complete, including the safety-critical
  pending-decision prompt (verified to raise/focus over a fullscreen window
  on a real Plasma session) and all four tray states
  (`Idle`/`Pending(n)`/`DaemonDown`/`RecentBlock`/`FilterOff`, the last
  three added 2026-07-12, see `.agent_native/agent_roadmap.md` item 9).
- `crates/snitchwatch-tauri/` + `web/` (the original shell) still exist and
  work, but are intentionally **not** what the Flatpak packages —
  `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` targets
  `snitchwatch-kirigami`. They're kept until a packaged release ships
  (explicit owner decision, 2026-07-11) — don't remove them before that.

**Component B (Bazzite scanner):** also done — Phases 4 (baseline design),
5 (userspace tier), 6 (privileged tier + Kirigami report UI) are all
code-complete. This section's original "not started, design-only" framing
below is entirely superseded.

**CI:** `.github/workflows/ci.yml` runs 4 jobs on every push/PR to `main`:
`check`/`test` (default-members, Ubuntu), `package-check` (packaging
artifact validation), and `kirigami` (builds/lints/tests
`snitchwatch-kirigami` in a Fedora container, since KF6 Kirigami packages
don't exist in Ubuntu's repos yet). All green as of the last commit.

**Update (2026-07-31): first real-hardware verification ran.** Live
opensnitchd v1.8.0 dial-in, AskRule→WS delivery, and the diagnostics
protocol are all confirmed working on a real Bazzite host — and the run
surfaced three real product findings, filed as issues #5 (daemon only
Pings when it has new stats events → the watchdog false-positives
DaemonDown on idle systems; blocks Step 5/6b pass conditions), #6
(EbpfSupport check can't see real eBPF module-load failure), and #7
(opensnitch isn't in Fedora/Bazzite repos — the recipe's install step and
both install docs were wrong; docs corrected, bluebuild recipe fix
pending). Detailed results:
`docs/superpowers/plans/2026-07-05-phase2-packaging.md` acceptance
section.

**Update (later 2026-07-31): all three findings are fixed and merged** —
#5 via PR #9 (liveness = any RPC activity + open Notifications stream;
re-verified live: idle daemon reads alive, down transition now fires in
~2s via stream-close with a consistent report, recovery pushes an
all-clear without a recheck), #7 via PR #10 (recipe/docs install the
upstream v1.8.0 release RPM), #6 via PR #11 (daemon-reported alerts
overlay onto the diagnostics checks — text-classified, since real v1.8.0
alerts are all `GENERIC`; alerts persist until a user-driven Recheck).
**Correction:** issue #6's "eBPF incompatible with kernel 6.19" premise
was a mis-diagnosis — the failure was rootless-container permissions;
under root, ebpf loads fine on 6.19 (see issue #6's post-close comment).
The shipped config keeps upstream's `ProcMonitorMethod: ebpf` (PR #10
briefly shipped `proc`; reverted). Also verified live along the way:
sustained interception under a real daemon + eBPF process monitoring.

**Correction (2026-07-31): the "verdict round-trip" claim above was
over-claimed — filed as issue #14.** A `tailscaled` connection was asked and
a `setVerdict` was sent back over WS promptly, but the daemon never actually
*applied* it: every real `AskRule` ended in `DeadlineExceeded` +
`Invalid rule received, applying default action`. Root cause: the bridge's
`verdict_to_rule` always set `operator: None` on the reply `Rule`, which
`vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` hard-rejects — so no
verdict from this bridge has ever been accepted by a real daemon. Fixed by
populating a real `Operator` per the verdict's scope (`ThisHost`/
`AnyHostOnDomain`/`AnyHost`) and by hardening `MockOpensnitchd::ask_rule` to
apply the daemon's real ~120s deadline and validate the returned `Rule`
shape, so this class of bug can't pass the sandbox suite silently again.
Live re-verification against a real daemon is still outstanding. Remaining
real-hardware items are listed in the phase2 plan's acceptance section —
mostly GUI-visual checks, the sustained-interception/queue-rules question,
and re-verifying the verdict round-trip now that issue #14 is fixed.

**What's actually left:**
1. **Real Bazzite hardware verification** — a bluebuild image build, a
   Flatpak build, a live `opensnitchd` dial-in, the closed-window
   fail-open proof, and the tray-state transitions (5 steps total). Full
   instructions: `docs/packaging/phase2-manual-verification-runbook.md`.
   See "Running on real hardware" below for the short version.
2. **Retiring `snitchwatch-tauri`/`web/`** — blocked on a packaged release
   actually shipping, not on any remaining code work.

Nothing else is blocked on more agent/sandbox work. If you're an agent
picking this up: don't re-derive the phase status from `IMPLEMENTATION_PROMPT.md`'s
original phase descriptions without reading each phase's own inline
"Update" note first — they're kept current.

---

## Running on real hardware

Quick-start for a Bazzite (or Fedora Silverblue/Kinoite-family) box. Full
detail, pass/fail conditions, and troubleshooting per step are in
`docs/packaging/phase2-manual-verification-runbook.md` — this is the short
version.

**Fastest path — just run the dev build, no packaging:**
```bash
git submodule update --init --recursive
just build                # needs protoc; Tauri needs webkit2gtk-4.1 dev headers
just kirigami-dev         # needs system Qt6 + KDE Frameworks 6 (Kirigami) dev packages
```
This runs the in-process bridge + Kirigami shell directly — no Flatpak, no
bluebuild image, nothing installed system-wide. Good enough to confirm the
GUI itself works on your hardware. It won't exercise real `opensnitchd`
unless you also point it at one (see README's "Running the bridge against
real opensnitchd").

**Full packaged install (batteries-included):**
```bash
bluebuild build packaging/bluebuild/recipe.yml    # bakes opensnitchd + fail-closed config
# ...rebase onto the resulting image per bluebuild's own instructions...
```

**Full packaged install (lightweight/DIY, layer onto stock Bazzite):**
Follow `docs/packaging/rpm-ostree-layering.md` step by step — it covers
`rpm-ostree install opensnitch`, the fail-closed config override, and
installing `packaging/systemd/snitchwatch-bridge.service` as your own user
service.

**GUI either way — build + install the Flatpak:**
```bash
python3 flatpak-cargo-generator.py Cargo.lock \
  -o packaging/flatpak/generated-cargo-sources.json
flatpak run org.flatpak.Builder --user --install --force-clean \
  build-dir packaging/flatpak/org.snitchwatch.Snitchwatch.yml
flatpak run org.snitchwatch.Snitchwatch
```

**Then verify it's actually working**, not just installed — the 5 steps in
`docs/packaging/phase2-manual-verification-runbook.md`:
1. Bluebuild image actually has `opensnitchd` enabled + the fail-closed
   config baked in.
2. Flatpak sandbox boundary actually holds (no stray network access, the
   Unix-socket filesystem permission actually crosses the sandbox).
3. A real `opensnitchd` dial-in round-trips a live `AskRule`.
4. Closing the GUI window does **not** kill the decision channel (the
   whole point of Phase 2 — the bridge runs as its own systemd `--user`
   service, decoupled from the GUI).
5. The tray icon actually reflects `DaemonDown`/`RecentBlock`/`FilterOff`
   in real time, not just in the sandbox's mocked tests.

Record results by flipping the checkbox in
`docs/superpowers/plans/2026-07-05-phase2-packaging.md`'s "Acceptance
criteria & verification status" section, same convention Task 7's
fullscreen-focus test used.

### Troubleshooting: GUI runs, but no visible connection to opensnitchd

**Update (2026-07-14):** this class of problem now surfaces *inside the
running GUI* — a warning banner plus a "Daemon Health" page — instead of
requiring the manual `/var/log/opensnitchd.log` triage below. See
`docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md`
for the design and `docs/superpowers/plans/2026-07-14-daemon-kernel-diagnostics.md`
for the implementation. The manual steps below remain accurate as a deeper
fallback (the in-app troubleshooting text is deliberately terser) and as
the only option before this feature shipped.

Symptom hit 2026-07-13 verifying on real hardware: `snitchwatch-kirigami`
builds and launches fine, but nothing happens and no error appears anywhere
in the GUI. Root cause is topology, not a bug: **`opensnitchd` is the gRPC
*client* — it dials out to the bridge, not the other way around** — so the
GUI has no direct line to `opensnitchd` at all; it only ever talks to the
bridge over the WS Unix socket. Two things have to line up for the dial-in
to happen:

1. `opensnitchd`'s own config (`/etc/opensnitchd/default-config.json` on
   the host) needs `Server.Address` pointing at the bridge, e.g.
   `127.0.0.1:50051`. The vendored default under
   `vendor/opensnitch/daemon/data/default-config.json`
   (`unix:///tmp/osui.sock`) is opensnitch's *own* built-in UI socket, not
   this bridge — leaving it as-is means opensnitchd never even attempts to
   dial the bridge.
2. The bridge has to actually be listening on that same address.
   `snitchwatch-bridge-cli` defaults to an **ephemeral port** unless
   `SNITCHWATCH_GRPC_BIND=127.0.0.1:50051` is set explicitly — the shipped
   `packaging/systemd/snitchwatch-bridge.service` sets this, but running
   `just run-bridge` by hand does not.

If either is off, there's silence by design — no error surfaces in
Snitchwatch because opensnitchd never connects to hand one to the bridge in
the first place. **Check `opensnitchd`'s own log**, not the GUI:

```bash
tail -f /var/log/opensnitchd.log
```

Fastest way to get a real signal:

```bash
# 1. Start the bridge with a fixed, known gRPC bind:
SNITCHWATCH_GRPC_BIND=127.0.0.1:50051 RUST_LOG=debug just run-bridge
# → watch for "GRPC_LISTEN_ADDR=127.0.0.1:50051" printed to stdout

# 2. Confirm opensnitchd's config actually points there:
grep -A2 '"Server"' /etc/opensnitchd/default-config.json

# 3. Restart opensnitchd and tail its log for the dial attempt/failure:
sudo systemctl restart opensnitchd
sudo tail -f /var/log/opensnitchd.log
```

Separately: `opensnitchd` isn't installed at all in the dev sandbox this
repo is normally worked in (no `opensnitchd` binary, and the sandbox isn't
even running systemd as PID 1) — that environment can only verify the GUI
launches, never a real daemon dial-in. Real-hardware verification is
mandatory for this step, not optional. A first attempt at compiling
`snitchwatch-tauri`/`snitchwatch-kirigami` on the reporting user's baremetal
box also failed outright (pre-dial-in, a separate build-environment issue —
missing system dev packages is the leading suspect per this file's
"Running on real hardware" prerequisites).

**Update (2026-07-30): baremetal build failure diagnosed and fixed.** It was
not missing dev packages — all prerequisites were present. `just build`
failed with both Kirigami crates' build scripts panicking:
`Conflicting include_prefixes for cxx-qt! Dependency cxx-qt-lib conflicts
with existing include path` (`cxx-qt-build 0.9.1`, `src/lib.rs:706`). Root
cause: in this box's containerized dev shell (toolbox/distrobox-style —
`systemctl` reports `offline` there), `/home/user` and `/var/home/user`
are the same directory via *bind mount* (verified: same device+inode,
`/home` a real directory) rather than the bare host's `/home → var/home`
symlink, so `fs::canonicalize` cannot unify the two spellings. (On the
bare host the symlink makes both spellings canonicalize identically and
this failure cannot happen.) Earlier builds ran from a `/home/user/…`
cwd and recorded `cxx-qt-lib`'s exported-include symlinks under that
spelling in `target/`; a later build from `/var/home/user/…` re-ran only
the consumer crates' build scripts, and cxx-qt-build's conflict check
(`canonicalize(source) == canonicalize(dest)`) saw the same physical
directory under two spellings and panicked. Fix:

```bash
cargo clean -p cxx-qt -p cxx-qt-lib -p cxx-qt-build \
  -p kirigami-spike -p snitchwatch-kirigami
just build   # from ONE consistent path spelling
```

**Gotcha to avoid recurrence:** always invoke cargo from the same path
spelling on this box (pick `/var/home/user/…` or `/home/user/…` and stick
with it — shells, IDEs, and agents included). Mixing them re-poisons
`target/` and the panic returns until the cxx-qt crates are cleaned again.

---

## Original 2026-07-04 handoff (history — decisions and rationale below are
## still accurate; status framing above supersedes anything below)

Two related but independent components, both targeting Bazzite. This
supersedes the original handoff brief — all four of its open questions are
now resolved (see below), and a scoped implementation prompt exists.

## Component A: App Firewall ("Snitchwatch") — this repo, pre-alpha

A Little Snitch–style per-process outbound firewall GUI on top of
OpenSnitch. Snitchwatch is a friendlier frontend, **not** a from-scratch
interception engine: opensnitchd dials into a bridge's gRPC `protocol.UI`
server, and the bridge translates to a Little-Snitch-v6-style WebSocket
protocol for the frontend.

**What's built (M0–M4 complete):**

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge lib: cache, translator, ws server, grpc client, blocklists
├── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
└── snitchwatch-tauri/       # Tauri 2 desktop shell: tray, notifications, autostart, wizard, crash log
tests/                        # bridge_protocol_test.rs, mock_opensnitchd, tauri_smoke/, web_smoke/
web/                          # vendored Little-Snitch-for-Linux-style frontend (rebranded)
```

Blocklist subscription (M4) is done: fetch → parse → materialize into
opensnitchd's `900-blocklist:<name>:` rule band, pushed live over WS.

## Component B: Immutable-OS Security Scanner — not started, design-only

Periodic anomaly/risk scanner for Bazzite's rpm-ostree/atomic model.
Userspace tier (default, no privilege) + on-demand privileged tier
(polkit/pkexec, **no persistent root daemon** — this property is load-bearing
and protected by decision #1 below). Zero code exists; three design docs do
(see "Design docs produced" below).

## The four original open questions — all resolved

1. **One app or two?** → **Two separate apps.** Component B's core security
   property (no persistent privileged daemon) can't coexist with merging
   into Component A's always-on bridge/daemon architecture. They share only
   a design system and, optionally, a signal channel (B reading A's
   connection log — not yet built, see Phase 5).
2. **Component A's interception layer** → **keep riding opensnitchd**
   indefinitely. Snitchwatch stays a friendlier opensnitchd frontend;
   building a native nfqueue/eBPF daemon is explicitly deferred.
3. **Component B's atomic-baseline problem** → **solved**, not just scoped.
   See `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`:
   delegates to `rpm-ostree status/db diff`, `rpm -V`, and OSTree's
   content-addressed object store rather than reimplementing manifest
   diffing. A 5-step classification tree (out-of-scope path → base-tree
   match → layered-package match → curated dynamic-path allowlist → else
   anomalous) replaces the original open question outright.
4. **Distribution model** → **bluebuild image (primary) + documented
   rpm-ostree layering (alternative) + Flatpak GUI.** Finding: the bridge
   needs no privileged access (no `CAP_NET_RAW`) — only opensnitchd does —
   so the GUI/bridge can be Flatpak-sandboxed. **Caveat discovered and
   fixed:** Flatpak isolates the network namespace, not just capabilities,
   so the original TCP-loopback assumption didn't actually cross the
   sandbox boundary. Fixed by moving the WS transport to a Unix domain
   socket gated by `--filesystem=xdg-run/snitchwatch` instead of
   `--share=network`. See
   `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`.

## Decisions made beyond the original four questions

- **GUI stack: Qt6/QML + Kirigami** (not GTK4/libadwaita as originally
  specified, not Tauri as currently built). Bazzite's default desktop is
  KDE Plasma, not GNOME — GTK4/libadwaita wouldn't have been native there
  either. This is a full rewrite of the current Tauri shell + vendored
  ~6,939-line web frontend. `cxx-qt` (the Rust↔Qt binding) researched and
  cleared with caveats: maintained by KDAB, KDE's own official docs host a
  Rust+Kirigami tutorial, but it's pre-1.0 and has an open build/test
  linking issue (GitHub #770) relevant to this repo's `tests/` convention.
  See `docs/superpowers/specs/2026-07-04-gui-stack-decision.md` and
  `2026-07-04-cxx-qt-feasibility-research.md`.
- **WS channel had no auth at all** — fixed via a shared-secret handshake
  token, now bundled with the Unix-socket transport fix above (Phase 1).
- **opensnitchd fails open by default** (`DefaultAction: allow` in
  `vendor/opensnitch/daemon/data/default-config.json:18`), confirmed via
  source, not assumed. Snitchwatch's shipped config should override this to
  `deny`.
- **Component B's privileged tier, itemized**: `chkrootkit` (not
  `rkhunter` — stale since 2018, no eBPF-rootkit coverage) for rootkit
  scanning; `/proc/cmdline` vs. `rpm-ostree kargs` for boot-parameter
  drift; loaded-module classification via the same base-tree/layered/
  anomalous model as the file-drift baseline. See
  `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`.

## Design docs produced this pass

| Doc | Purpose |
|---|---|
| `AUDIT.md` | Stage-1 audit: the four decisions + independent critique |
| `IMPLEMENTATION_PROMPT.md` | 6-phase, branch-by-branch implementation plan |
| `docs/superpowers/specs/2026-07-04-gui-stack-decision.md` | GTK4 vs. Tauri vs. Qt/Kirigami options analysis |
| `docs/superpowers/specs/2026-07-04-cxx-qt-feasibility-research.md` | Rust↔Qt binding maturity research |
| `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md` | 18-task ordered UI migration plan |
| `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md` | Sandbox network/filesystem boundary research |
| `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md` | Component B's atomic-baseline classification design |
| `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md` | Rootkit/kernel-audit tool selection for Component B |

(Later plan docs, not listed here individually, live in
`docs/superpowers/plans/` — the naming convention is
`YYYY-MM-DD-<slug>.md`; browse that directory for anything after
2026-07-04, including the Phase 2 packaging plan, the Kirigami rewrite's
final status, both scanner-tier plans, the Phase 6 report-UI plan, and the
three 2026-07-12 tray-state plans.)
