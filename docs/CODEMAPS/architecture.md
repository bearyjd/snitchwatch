<!-- Generated: 2026-11-06 | Files scanned: 62 | Token estimate: ~880 -->
# Architecture

Two independent products, one Cargo workspace (`Cargo.toml`: 11 crates + `tests/mock_opensnitchd`, `tests/integration`). No shared daemon, unit or privilege model (CLAUDE.md, settled decision 1).

## Component A: Snitchwatch (GUI over opensnitchd)
```
 opensnitchd (root, Go, vendor/opensnitch v1.8.0)
     | gRPC client dials IN (ui.proto `Ui` service)
     v   Unix sock /run/snitchwatch/opensnitchd.sock (system) | TCP 127.0.0.1 (per-user, "not deny-safe")
 snitchwatch-bridge (lib) hosted by snitchwatch-bridge-cli (bin)
     |  grpc_server::UiService -> translator -> cache/rules/profiles/blocklists/curated
     |  ws_server::WsServer  (axum over hyper, Unix socket)
     v   WS /stream, first text frame = handshake token
 GUI: snitchwatch-kirigami (Qt6/QML, cxx-qt) [target]
      snitchwatch-tauri + web/ [legacy, kept during migration]
```
- Rule writes go to the daemon as `Notifications` messages; only `ChangeRule`/`DeleteRule` are allowed (`crates/snitchwatch-bridge/src/daemon_commands.rs:108`).
- `SNITCHWATCH_SYSTEM_BRIDGE=1` selects the systemd-activated system bridge (`crates/snitchwatch-bridge-cli/src/main.rs:89-93`; socket paths `activation.rs:10-12`; `run_system` `lib.rs:139`).
- Kirigami only connects to a bridge (`crates/snitchwatch-kirigami/src/bridge_runtime.rs`); Tauri embeds one in-process.

## Component B: Bazzite scanner
```
 scanner-cli (userspace, one-shot) -> scanner-core (checks, SystemInspector, scans.db)
 Deep scan: GUI -> [Flatpak] scanner-broker (session D-Bus org.snitchwatch.Scanner)
                -> pkexec -> scanner-privileged (root, one-shot, watchdog) -> JSON on stdout
            GUI -> [native] pkexec scanner-privileged (kirigami src/scanner.rs:98)
```
No persistent privileged process (`crates/scanner-privileged/src/lib.rs`). Only Kirigami links scanner-broker. `scanner-core`'s outbound check is a deferred stub (`checks/outbound.rs`).

## Trust boundaries
| Boundary | Guard |
|---|---|
| daemon -> bridge | system: root-only Unix socket; per-user: TCP loopback |
| GUI -> bridge | token as first WS frame, `SO_PEERCRED` uid (`ws_server.rs:161`). User mode: dir 0700, socket 0600. System mode: `/run/snitchwatch` 0711 root:root (`packaging/system/snitchwatch.conf:3`), GUI socket 0660 root:snitchwatch-ui (`snitchwatch-system-bridge-gui.socket:8-10`) |
| Flatpak -> host | `--filesystem=xdg-run/snitchwatch`; broker takes no exe/args/env |
| user -> root scan | polkit `packaging/polkit/org.snitchwatch.scanner.policy` |

## Prompt flow
`AskRule` (`grpc_server.rs:357`) -> deferred slot + `InsertConnectionRows` broadcast -> user verdict -> `translator::upstream::apply` (`upstream.rs:47`) -> rule reply. Unanswered prompts time out (`ANSWER_TIMEOUT`).

See: bridge.md, gui.md, scanner.md, data.md, dependencies.md.
