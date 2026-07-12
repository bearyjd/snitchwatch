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
