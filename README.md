# Snitchwatch

A Little Snitch–style network firewall GUI for Linux, on top of OpenSnitch.

## Status

Pre-alpha, but functionally complete end to end. The headless bridge
translates between OpenSnitch's gRPC protocol and a Little-Snitch-style
protocol, with full test coverage and an AskRule round-trip end-to-end test
against an in-process mock daemon. The native desktop shell is
**`snitchwatch-kirigami`** (Qt6/QML + Kirigami) — see "Try the Kirigami
shell" below; `snitchwatch-tauri`/`web/` are an earlier shell kept in the
tree only until a packaged release ships. Packaging (bluebuild image,
rpm-ostree layering, Flatpak) and the Bazzite security scanner (Component
B, `scanner-core`/`scanner-cli`/`scanner-privileged`) are also code-complete
— remaining work is real-hardware verification, tracked in
`docs/packaging/phase2-manual-verification-runbook.md` and
`IMPLEMENTATION_PROMPT.md`.

See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` for the
original design and `IMPLEMENTATION_PROMPT.md` for the full phase-by-phase
status.

## Building

Requires: Rust 1.75+ and a protobuf compiler (`protoc`).

```bash
git submodule update --init --recursive
just build
```

## Testing

```bash
just test                    # full workspace: unit + integration tests
just test-bridge             # just the bridge crate unit tests
just check                   # cargo check + clippy -D warnings
just doctor                  # checks for missing one-time setup steps below
```

### Playwright smoke suites (one-time setup)

`tests/web_smoke` and `tests/tauri_smoke` are Playwright suites and need a
one-time browser install before they'll run — `just web-smoke`/
`just tauri-smoke` will fail with a "missing browser" error otherwise, which
is a setup gap, not a code regression:

```bash
just web-smoke-install       # once, before `just web-smoke`
just tauri-smoke-install     # once, before `just tauri-smoke`
```

Run `just doctor` to check whether these have already been done.

The headline integration test is `ask_rule_round_trip_full` in
`tests/bridge_protocol_test.rs`: it spins up an in-process mock
opensnitchd, boots the bridge, drives a full AskRule → pending row →
setVerdict → NotificationReply round trip through the WebSocket.

## Architecture

The bridge is a Rust workspace member that exposes:

- **gRPC `protocol.UI` server** (TCP loopback) — opensnitchd dials in here
  as the gRPC client. The bridge implements `Ping`, `AskRule`, `Subscribe`,
  `PostAlert`, and the bidi `Notifications` stream. `AskRule` is a blocking
  unary handler: the bridge inserts a pending row into its in-memory cache,
  broadcasts it on the WebSocket, awaits the user verdict via a `oneshot`,
  then translates the verdict into a `Rule` reply.
- **WebSocket+HTTP server** (Unix domain socket) — the front-end (vendored
  LS-for-Linux UI in M2, Tauri shell in M3) connects to `/stream` and
  exchanges Little Snitch v6 protocol messages with the bridge; `/`,
  `/assets/*`, and the SPA fallback serve the static frontend. This server
  binds a Unix domain socket under `$XDG_RUNTIME_DIR/snitchwatch/` (mode
  0700 dir, 0600 socket file) rather than TCP loopback — a Flatpak-sandboxed
  GUI gets its own private network namespace and can never reach TCP
  loopback on the host regardless of auth, but a Unix socket under
  `$XDG_RUNTIME_DIR` is reachable via the well-precedented
  `--filesystem=xdg-run/snitchwatch` Flatpak permission. A fresh
  shared-secret **handshake token** is generated at startup and written
  alongside the socket (`$XDG_RUNTIME_DIR/snitchwatch/token`, mode 0600); a
  client must send it as the first WS text frame on `/stream` before the
  bridge treats the connection as trusted (the Unix socket's `SO_PEERCRED`
  gives a verified UID essentially for free, but this token is kept as an
  additional, simpler-to-reason-about layer on top of that, not the sole
  guard).

opensnitchd's `Server.Address` config tells it where to dial the gRPC
server. The bridge publishes the socket, token path, and gRPC address on
stdout at startup as `GRPC_LISTEN_ADDR=...`, `WS_SOCKET_PATH=...`, and
`WS_TOKEN_PATH=...`.

## Running the bridge against real opensnitchd

Start opensnitchd in a rootful podman container (it needs `NET_ADMIN`
plus host network and PID namespaces to see syscalls), then run the
bridge:

```bash
podman run -d --rm \
    --name opensnitchd-dev \
    --privileged --network=host --pid=host \
    --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
    docker.io/evilsocket/opensnitch:latest

just run-bridge
```

The bridge prints `GRPC_LISTEN_ADDR=127.0.0.1:NNNNN`,
`WS_SOCKET_PATH=/run/user/<uid>/snitchwatch/bridge.sock`, and
`WS_TOKEN_PATH=/run/user/<uid>/snitchwatch/token` to stdout on startup. Set
opensnitchd's `default-config.json` `Server.Address` field to the bridge's
`GRPC_LISTEN_ADDR` (e.g. `127.0.0.1:50051`) so the daemon dials in.

You can poke the WebSocket with `websocat`'s Unix-socket mode, presenting
the token as the first line so the bridge accepts the connection:

```bash
TOKEN=$(cat "$XDG_RUNTIME_DIR/snitchwatch/token")
{ printf '%s\n' "$TOKEN"; cat; } | websocat --unix-listen -t \
    ws-c:unix:"$XDG_RUNTIME_DIR/snitchwatch/bridge.sock":/stream
```

(the exact `websocat` invocation for dialing a Unix socket as a WS client is
`websocat ws-c:unix:<path>:/stream` — check `websocat --help` for your
installed version's exact Unix-socket flag spelling.)

Environment variables:

- `SNITCHWATCH_GRPC_BIND` — gRPC bind address (default `127.0.0.1:50051`)
- `SNITCHWATCH_WS_SOCKET` — WS Unix domain socket path (default
  `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`)
- `RUST_LOG` — tracing filter, e.g. `info`, `snitchwatch_bridge=debug`

## Try the Tauri shell (earlier shell, kept until Kirigami's release ships)

For the shell that actually ships, see "Try the Kirigami shell" below. This
Tauri+`web/` shell remains in the tree and works, but is being retired once
a packaged Kirigami release ships. After installing the workspace tooling
(`cargo`, `just`, optional Playwright for the smoke suite):

```bash
just tauri-dev
```

A native Snitchwatch window opens. The bridge runs in-process, binding its
WS+HTTP server on a Unix domain socket (see Architecture above), and a
small local loopback proxy (`snitchwatch_tauri::loopback_proxy`) bridges
that socket back to `127.0.0.1:3031` so the webview's `http://` URL keeps
working unchanged — you can still attach a browser tab there for debugging,
same as before. The proxy transparently presents the handshake token on the
webview's behalf, so no separate auth step is needed to use the app. The
system tray shows the current state — hover for a tooltip, right-click for
the menu.

### Autostart

Snitchwatch can launch at login automatically. Toggle from
**Settings → Start with system**, which writes
`~/.config/autostart/snitchwatch.desktop`. Disabling removes the file.

### Crash log

Panics are written to `$XDG_STATE_HOME/snitchwatch/crash.log` (default
`~/.local/state/snitchwatch/crash.log`). View the last 200 lines from the
**Diagnostics** tab.

### Privacy

The pending-decision dialog's insight panel can show reverse-DNS and RDAP
(online registration) info for a connection's remote IP. Reverse DNS always
uses the system resolver. RDAP queries a third-party service (`rdap.org`)
with the remote IP, so it is **opt-in and off by default** — enable it from
**Settings → Diagnostics → Online research (RDAP) in decision dialog**. A
Flatpak-sandboxed install has no network access for the GUI at all (see
"Install on Bazzite" above), so online research there requires a
non-sandboxed install or an explicit network permission grant.

## Try the Kirigami shell

`snitchwatch-kirigami` (Qt6/QML + Kirigami, via `cxx-qt`) is the shell that
actually ships — the Flatpak manifest builds it, not `snitchwatch-tauri`.
It's a native match for Bazzite's default KDE Plasma desktop and has
feature parity with the Tauri+`web/` shell above, including the same
autostart/crash-log/RDAP/coexistence-check surfaces (**Settings &
Diagnostics** page) and the same pending-decision safety behavior
(verified to raise/focus over a fullscreen window on a real Plasma
session).

Requires system Qt6 + KDE Frameworks 6 (Kirigami) dev packages — this is
why `kirigami-spike`/`snitchwatch-kirigami` are excluded from the workspace
`default-members` (a plain `cargo build`/`just build` won't touch them).

```bash
just kirigami-dev
```

Everything else — autostart, crash log, privacy/RDAP opt-in, `opensnitch-ui`
coexistence — works the same way as documented above for the Tauri shell,
just reached from this shell's own **Settings & Diagnostics** page instead.

## M4 — Subscribe to a blocklist

Snitchwatch ships its own blocklist subscription manager. To smoke-test it
end-to-end against the local fixture set:

```bash
just blocklist-fixture-server &       # serves tests/fixtures/blocklists/ on :8731
cargo run -p snitchwatch-bridge-cli   # bridge boots its WS socket under $XDG_RUNTIME_DIR/snitchwatch/
```

In another terminal, send a `subscribeBlocklist` action over the WS (token
first, per the Unix-socket handshake described above):

```bash
TOKEN=$(cat "$XDG_RUNTIME_DIR/snitchwatch/token")
{ printf '%s\n' "$TOKEN"; printf '%s\n' '{"action":"subscribeBlocklist","url":"http://127.0.0.1:8731/domains-tiny.txt"}'; cat; } \
    | websocat ws-c:unix:"$XDG_RUNTIME_DIR/snitchwatch/bridge.sock":/stream
```

You should immediately see two server messages: `setBlocklists` (with the new
subscription) and `setBlocklistEntries` (with the parsed hosts). The bridge
also pushes 5 deny rules into opensnitchd in the `900-blocklist:domains-tiny:`
band — visible via `opensnitchd-cli list-rules` if you have a real daemon
attached.

To run the blocklist test suite in isolation:

```bash
just test-blocklists
```

## Install on Bazzite (M5 / Phase 2 packaging)

Snitchwatch installs on Bazzite (or any Universal Blue / immutable Fedora)
host two ways. Both are supported — pick by taste, not by tier:

- **Batteries-included** — a signed custom Bazzite image with `opensnitchd`
  baked in and enabled from first boot. Recipe:
  [`packaging/bluebuild/recipe.yml`](packaging/bluebuild/recipe.yml). Build
  with the [`bluebuild`](https://blue-build.org) CLI.
- **Lightweight / DIY** — layer `opensnitchd` onto stock Bazzite with
  `rpm-ostree`. Step-by-step walkthrough:
  [`docs/packaging/rpm-ostree-layering.md`](docs/packaging/rpm-ostree-layering.md).

In both cases the GUI ships as a Flatpak
([`packaging/flatpak/org.snitchwatch.Snitchwatch.yml`](packaging/flatpak/org.snitchwatch.Snitchwatch.yml))
and the bridge runs as a systemd `--user` service
([`packaging/systemd/snitchwatch-bridge.service`](packaging/systemd/snitchwatch-bridge.service)).
The Flatpak reaches the host-side bridge over the Phase 1 Unix domain socket
via `--filesystem=xdg-run/snitchwatch` — it holds **no** network permission
(`--share=network` is deliberately absent; a Flatpak's private network
namespace can't reach host loopback anyway, and that grant would open full
internet access rather than scoped loopback). See
[`packaging/README.md`](packaging/README.md) for the full architecture.

### Fail-closed by default

Upstream OpenSnitch ships `DefaultAction: allow`, so the daemon silently
**allows** all traffic whenever no UI client is connected. Snitchwatch's
packaging overrides this with `DefaultAction: deny`
([`packaging/bluebuild/files/system/etc/opensnitchd/default-config.json`](packaging/bluebuild/files/system/etc/opensnitchd/default-config.json)):
a firewall whose whole premise is "ask before allowing" should fail **closed**
when its decision channel is down. The bridge runs as its own `--user`
service precisely so that *closing the GUI window* is not mistaken for *the
decision channel being down* — the daemon only reaches its deny default on a
genuine bridge outage.

### Coexistence with upstream `opensnitch-ui`

Snitchwatch replaces the upstream OpenSnitch GUI. If `opensnitch-ui` is also
installed and autostarting, the two will contend for the daemon's UI gRPC
channel (and the vendored `ui.proto` may drift from a differently-versioned
upstream). Install only the daemon (`opensnitch`), not `opensnitch-ui`, and
disable any existing `~/.config/autostart/opensnitch_ui.desktop`. The
rpm-ostree walkthrough documents the detect-and-disable step, and the
Kirigami shell's **Settings & Diagnostics** page runs this same check
automatically at runtime (warns if `opensnitch-ui` is installed or its
autostart entry is present) — see `crates/snitchwatch-kirigami/src/coexistence.rs`.

## Workspace layout

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge library (cache, translator, ws server, grpc client)
├── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
├── snitchwatch-kirigami/    # Qt6/QML + Kirigami desktop shell (cxx-qt) — the shell that ships
├── snitchwatch-tauri/       # earlier Tauri 2 shell — kept until a packaged release ships
├── kirigami-spike/          # throwaway cxx-qt feasibility spike (Phase 3a), not shipped
├── scanner-core/            # Component B: userspace-tier Bazzite security scanner
├── scanner-cli/             # Component B: userspace-tier CLI orchestrator
└── scanner-privileged/      # Component B: on-demand privileged-tier scanner (polkit/pkexec)
tests/
├── bridge_protocol_test.rs  # the round-trip integration test
├── integration/             # crate that owns the integration test
└── mock_opensnitchd/        # in-process gRPC mock with scripted events
```

`kirigami-spike` and `snitchwatch-kirigami` need system Qt6 + KDE Frameworks 6
dev packages and are excluded from `default-members` — see "Try the
Kirigami shell" above and `just build`/`just check` won't touch them.
Component B (the scanner crates) is an unrelated product sharing this repo
and, at most, a design system with Component A — see `CLAUDE.md`'s
"Settled architecture decisions" for why they don't share a daemon.

## License

GPL-2.0
