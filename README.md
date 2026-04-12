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

The bridge prints `WS_LISTEN_ADDR=127.0.0.1:NNNNN` to stdout on startup.
You can poke it with `websocat`:

```bash
websocat ws://127.0.0.1:NNNNN/stream
```

Environment variables:

- `SNITCHWATCH_GRPC` — opensnitchd gRPC endpoint (default `http://127.0.0.1:50051`)
- `SNITCHWATCH_WS_BIND` — WebSocket bind address (default `127.0.0.1:3031`)
- `RUST_LOG` — tracing filter, e.g. `info`, `snitchwatch_bridge=debug`

## Try it in your browser (M2 milestone)

Snitchwatch's M2 milestone serves the vendored Little Snitch for Linux UI directly
from the bridge — no Tauri shell yet, just a browser tab.

1. Build the bridge once:
   ```bash
   cargo build -p snitchwatch-bridge-cli
   ```
2. Run it:
   ```bash
   cargo run -p snitchwatch-bridge-cli
   ```
3. The bridge prints its listen address on startup:
   ```text
   WS_LISTEN_ADDR=127.0.0.1:3031

   → open http://127.0.0.1:3031/ in your browser
   ```
4. Open that URL in Firefox (or any modern browser). The Connections, Rules,
   Blocklists, and Traffic tabs render against the vendored SPA.

To exercise the live AskRule round trip without a real opensnitchd, point the
helper binary at the bridge's gRPC address:

```bash
cargo run --quiet --manifest-path tests/web_smoke/helpers/Cargo.toml -- \
  --grpc 127.0.0.1:50321 --process /usr/bin/curl --host example.com --port 443
```

The browser tab shows the pending row. Click Allow or Deny in the inspector and
the helper exits with the synthesized rule.

## Workspace layout

```text
crates/
├── snitchwatch-proto/       # generated tonic/prost bindings for opensnitchd's ui.proto
├── snitchwatch-spike/       # M0 spike binary that probes a live daemon
├── snitchwatch-bridge/      # headless bridge library (cache, translator, ws server, grpc client)
└── snitchwatch-bridge-cli/  # thin orchestrator (lib::run + main.rs)
tests/
├── bridge_protocol_test.rs  # the round-trip integration test
├── integration/             # crate that owns the integration test
└── mock_opensnitchd/        # in-process gRPC mock with scripted events
```

## License

GPL-2.0
