# Snitchwatch

A Little Snitch–style network firewall GUI for Linux, on top of OpenSnitch.

## Status

Pre-alpha. Plan 1 (bridge foundation) is complete: the headless bridge
crate translates between OpenSnitch's gRPC protocol and Little Snitch's
WebSocket protocol, with full test coverage and an AskRule round-trip
end-to-end test against an in-process mock daemon.

See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` for the design,
and `docs/superpowers/plans/2026-04-10-bridge-foundation.md` for the Plan 1
task breakdown.

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
```

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

## Try it as a native desktop app (M3)

After installing the workspace tooling (`cargo`, `just`, optional Playwright for the smoke suite):

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

## Workspace layout

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge library (cache, translator, ws server, grpc client)
├── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
└── snitchwatch-tauri/       # Tauri 2 desktop shell (tray, notifications, autostart, wizard)
tests/
├── bridge_protocol_test.rs  # the round-trip integration test
├── integration/             # crate that owns the integration test
└── mock_opensnitchd/        # in-process gRPC mock with scripted events
```

## License

GPL-2.0
