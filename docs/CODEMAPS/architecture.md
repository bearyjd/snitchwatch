<!-- Generated: 2026-11-25 | Files scanned: 1199 | Token estimate: ~950 -->
# Architecture

Two independent products, one Cargo workspace (`Cargo.toml`: 10 crates + `tests/mock_opensnitchd`, `tests/integration`). No shared daemon, unit or privilege model (CLAUDE.md, settled decision 1).

## Component A: Snitchwatch (GUI over opensnitchd)
```
 opensnitchd (root, Go, vendor/opensnitch master a1353848; installed daemon is the v1.8.0 RPM)
     | gRPC client dials IN (ui.proto `Ui` service)
     v   Unix sock /run/snitchwatch/opensnitchd.sock (system) | $XDG_RUNTIME_DIR/snitchwatch-grpc/ (per-user) | TCP loopback (dev only, SNITCHWATCH_DEV_TCP=1)
 snitchwatch-bridge (lib) hosted by snitchwatch-bridge-cli (bin)
     |  grpc_server::UiService -> translator -> cache/rules/profiles/blocklists/curated
     |  ws_server::WsServer  (axum + hyper, Unix socket)
     v   WS /stream, first text frame = handshake token
 GUI: snitchwatch-kirigami (Qt6/QML, cxx-qt) [the only shell]
```
- Rule writes go to the daemon as `Notifications` messages; only `ChangeRule`/`DeleteRule` are allowed (`crates/snitchwatch-bridge/src/daemon_commands.rs:144`).
- `SNITCHWATCH_SYSTEM_BRIDGE=1` selects the systemd-activated system bridge (`crates/snitchwatch-bridge-cli/src/main.rs:93-98`; socket paths `activation.rs:13-15`; `run_system` `lib.rs:183`).
- Kirigami only connects to a bridge (`crates/snitchwatch-kirigami/src/bridge_runtime.rs`); it never starts one.

## Component B: Bazzite scanner
```
 scanner-cli (userspace, one-shot) -> scanner-core (checks, SystemInspector, scans.db)
 Deep scan: GUI -> [Flatpak] scanner-broker (session D-Bus org.snitchwatch.Scanner)
                -> pkexec -> scanner-privileged (root, one-shot, watchdog) -> JSON on stdout
            GUI -> [native] scanner_broker::run_native -> pkexec (kirigami src/scanner.rs:148-149; broker lib.rs:90)
```
`scanner-privileged` is a transient binary, not a daemon (`crates/scanner-privileged/src/lib.rs:9-10`). Only Kirigami links scanner-broker. `scanner-core`'s outbound check is a deferred stub (`checks/outbound.rs`).

## Trust boundaries
| Boundary | Guard |
|---|---|
| daemon -> bridge | system: root-only Unix socket; per-user: Unix socket, dir 0700, socket 0600 (`user_socket.rs:14-15`), trusted no more than TCP (`daemon_commands.rs:122-131`) |
| GUI -> bridge | token must match on the first WS frame (`await_handshake`, `ws_server.rs:198`); the `SO_PEERCRED` uid is captured for logging only (`ws_server.rs:67-71,156`). User mode: dir 0700 (`ws_server.rs:119`), socket 0600 (`:125`). System mode: `/run/snitchwatch` 0711 root:root (`packaging/system/snitchwatch.conf:3`), GUI socket 0660 root:snitchwatch-ui (`snitchwatch-system-bridge-gui.socket:8-10`) |
| Flatpak -> host | `--filesystem=xdg-run/snitchwatch`; `run_deep_scan` rejects any arguments (`scanner-broker lib.rs:261`) |
| user -> root scan | polkit `packaging/polkit/org.snitchwatch.scanner.policy` |

## Prompt flow
`AskRule` (`grpc_server.rs:443`) -> deferred slot + `InsertConnectionRows` broadcast -> user verdict -> `translator::upstream::apply` (`upstream.rs:47`) -> rule reply. Unanswered prompts time out (`ANSWER_TIMEOUT`).

Connection history is off by default; the bridge CLI starts its control gate (`bridge-cli lib.rs:211`), see bridge.md.

See: bridge.md, gui.md, scanner.md, data.md, dependencies.md.
