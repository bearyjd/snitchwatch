# Phase 2 — Distribution & fail-open stance (Component A)

> Plan doc for Phase 2 of `IMPLEMENTATION_PROMPT.md`. Follows the
> `docs/superpowers/plans/` convention. Blocks on Phase 1 (Unix-socket + token
> transport), which has landed on `feat/snitchwatch-shell-and-release`.

**Goal:** ship Snitchwatch on Bazzite two ways — batteries-included
(custom bluebuild image with `opensnitchd` baked in) and lightweight
(rpm-ostree-layer `opensnitchd` onto stock Bazzite) — deliver the GUI as a
Flatpak in both cases, run the bridge as a host-side systemd `--user` service
decoupled from the GUI window, and fail **closed** when the decision channel
is down.

## Decisions & rationale

### 1. The bridge is host-side, the Flatpak is GUI-only

`IMPLEMENTATION_PROMPT.md` Phase 2 and the flatpak-feasibility research both
land on the same architecture: the privileged `opensnitchd` and the
unprivileged `snitchwatch-bridge-cli` stay **host-side**; only the GUI shell
is sandboxed. The GUI reaches the bridge over a **Unix domain socket** under
`$XDG_RUNTIME_DIR/snitchwatch/`, granted via `--filesystem=xdg-run/snitchwatch`.

> **Reconciliation note.** The task brief phrased the Flatpak as being "for
> the GUI+bridge". That is read here as "for Component A", not "the bridge
> process lives inside the sandbox" — the authoritative feasibility doc's
> Verdict and Phase 2's own fail-open resolution both keep the bridge
> host-side. Putting the bridge inside the sandbox would reverse the
> boundary-crossing direction (host `opensnitchd` would have to dial *into* a
> sandboxed gRPC server), which depends on unverified opensnitchd UDS support
> — explicitly the thing the feasibility doc warns against betting on.

### 2. No `--share=network`

A Flatpak's default sandbox has a private, isolated network namespace with its
own private loopback device, so it cannot reach host `127.0.0.1` regardless of
auth; `--share=network` grants full internet access, not scoped loopback. The
Unix socket + `--filesystem=xdg-run/snitchwatch` is the only mechanism that
works — the same one virt-manager's Flathub manifest uses for libvirtd.

### 3. Fail **closed** — `DefaultAction: deny`

Confirmed in-tree: `vendor/opensnitch/daemon/data/default-config.json:18`
ships `"DefaultAction": "allow"`, and that value is exactly what opensnitchd
applies when no UI client is connected (`ui/config_utils.go:178-180`,
`main.go:431-433`). Out of the box the daemon therefore silently *allows* all
traffic once the bridge/GUI goes away. Since `DefaultAction` is a plain JSON
config key, Snitchwatch's packaging ships a hardened
`default-config.json` with `"DefaultAction": "deny"` instead. The vendored
submodule is left untouched; the override is a packaging artifact
(`packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`)
installed to `/etc/opensnitchd/default-config.json` by both install paths.

### 4. Bridge as a systemd `--user` service (the fail-open fix)

`opensnitchd` treats "GUI window closed" identically to "decision channel
down". Decoupling the bridge into its own `--user` service
(`packaging/systemd/snitchwatch-bridge.service`) means closing the GUI window
no longer kills the AskRule path — the daemon only reaches its fail-closed
default on a genuine bridge outage. The unit pins
`SNITCHWATCH_GRPC_BIND=127.0.0.1:50051` so the daemon's `Server.Address` has a
fixed target (the bridge-cli otherwise defaults to an ephemeral port).

## Rust plumbing touched

- `crates/snitchwatch-bridge-cli/src/main.rs` — added SIGTERM handling
  (`wait_for_shutdown_signal`) alongside the existing SIGINT/Ctrl-C path.
  `systemctl --user stop` sends SIGTERM; without this arm the bridge would be
  killed by the default disposition and skip its clean `bridge.shutdown()`.
  The stable gRPC bind is supplied by the unit's `Environment=` (the existing
  `SNITCHWATCH_GRPC_BIND` env var already supports it — no code change needed
  there).

## Artifacts

| Path | What |
| --- | --- |
| `packaging/bluebuild/recipe.yml` | batteries-included image recipe |
| `packaging/bluebuild/files/system/etc/opensnitchd/default-config.json` | fail-closed daemon config (canonical) |
| `packaging/flatpak/org.snitchwatch.Snitchwatch.yml` | GUI-only Flatpak manifest (no `--share=network`) |
| `packaging/flatpak/org.snitchwatch.Snitchwatch.desktop` | desktop entry |
| `packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml` | AppStream metadata |
| `packaging/systemd/snitchwatch-bridge.service` | host-side bridge `--user` unit |
| `packaging/README.md` | packaging overview |
| `docs/packaging/rpm-ostree-layering.md` | lightweight install walkthrough |

## Acceptance criteria & verification status

- [x] Bluebuild recipe installs + enables `opensnitchd`, ships
  `Server.Address: 127.0.0.1:50051`. **YAML validated; image build needs a
  real host (see below).**
- [x] `DefaultAction: deny` shipped in the daemon config. **JSON validated.**
- [x] Flatpak manifest grants `--filesystem=xdg-run/snitchwatch`, does **not**
  grant `--share=network`. **YAML validated + asserted by a Rust shape test.**
- [x] Bridge systemd `--user` unit decoupled from the GUI window.
  **`systemd-analyze verify` clean where available.**
- [x] rpm-ostree layering walkthrough documented end-to-end.
- [x] README updated with both install paths as "batteries-included" vs
  "lightweight/DIY".
- [ ] **Not verifiable in the CI sandbox:** actual `bluebuild build`,
  `flatpak-builder` run, live opensnitchd dial-in, and the closed-window
  AskRule round-trip on a real Bazzite host. These need tooling and a host
  the sandbox lacks; step-by-step manual verification instructions for all
  four are in
  [`../../packaging/phase2-manual-verification-runbook.md`](../../packaging/phase2-manual-verification-runbook.md).
  **Resolved 2026-07-11:** the Flatpak manifest now packages
  `snitchwatch-kirigami` against `org.kde.Platform`, not
  `snitchwatch-tauri`/`web/` against `org.gnome.Platform` — Kirigami is the
  settled GUI stack and has feature parity plus a passing Task 7
  fullscreen-focus test, so it's the correct release target. The manifest,
  desktop entry, and metainfo were updated accordingly; `crates/
  snitchwatch-tauri/` and `web/` remain in the repo but are intentionally not
  what this manifest builds.

## Cross-cutting (per prompt's closing notes)

- `opensnitch-ui` coexistence: documented a detect-and-disable step in the
  rpm-ostree doc and a README callout (running two UI clients against one
  daemon contends for the UI gRPC channel; `ui.proto` may also drift).
  **Update 2026-07-11:** also a real runtime check now, not just docs —
  `crates/snitchwatch-kirigami/src/coexistence.rs`, surfaced on the
  Diagnostics page (checks `rpm -q opensnitch-ui` and the upstream
  autostart entry, warns via a `Kirigami.InlineMessage` if either is
  present).
