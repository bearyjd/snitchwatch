# Linux App Firewall + Bazzite Security Scanner — Handoff (updated 2026-07-04)

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

## Next step

Implementation, phase by phase per `IMPLEMENTATION_PROMPT.md`, using
Sonnet/Opus subagents (not Fable — Fable's role was research/design only,
now complete for this pass). Start with **Phase 1** (Unix domain socket +
auth token in `snitchwatch-bridge`) — it's unblocked and blocks all
packaging work. Two stop-and-ask gates remain inside the phases themselves:
Phase 3a's `cxx-qt` spike (go/no-go before the full Kirigami rewrite) and
Phase 4 is already resolved so Phase 5 is unblocked once Phase 4's design
doc is read by whoever implements it.
