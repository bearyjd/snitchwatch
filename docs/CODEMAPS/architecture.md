<!-- Generated: 2026-11-06 | Files scanned: 62 | Token estimate: ~950 -->
# Architecture

Two independent products, one Cargo workspace (`Cargo.toml`, 11 crates + `tests/mock_opensnitchd`, `tests/integration`). No shared daemon, unit or privilege model (CLAUDE.md, settled decision 1).

## Component A: Snitchwatch (GUI over opensnitchd)
```
 opensnitchd (root, Go, vendor/opensnitch v1.8.0)
     | gRPC client dials IN (ui.proto `Ui` service)
     v   Unix sock /run/snitchwatch/opensnitchd.sock (system) | TCP 127.0.0.1 (per-user, "not deny-safe")
 snitchwatch-bridge (lib) hosted by snitchwatch-bridge-cli (bin)
     |  grpc_server::UiService -> translator -> cache/rules/profiles/blocklists/curated
     |  ws_server::WsServer  (axum over hyper, Unix socket)
     v   WS /stream, first text frame = handshake token
 GUI client: snitchwatch-kirigami (Qt6/QML, cxx-qt)   [target]
             snitchwatch-tauri + web/ (WebKit webview) [legacy, kept during migration]
```
- Rule writes go back to the daemon as `Notifications` stream messages; only `ChangeRule`/`DeleteRule` are allowed (`crates/snitchwatch-bridge/src/daemon_commands.rs:108`).
- Per-user bridge vs system bridge: `SNITCHWATCH_SYSTEM_BRIDGE=1` selects systemd-activated sockets (`crates/snitchwatch-bridge-cli/src/activation.rs:10-12`); entry `run_system` at `crates/snitchwatch-bridge-cli/src/lib.rs:139`.
- Kirigami never starts a bridge; it only connects (`crates/snitchwatch-kirigami/src/bridge_runtime.rs`). Tauri embeds one in-process (`crates/snitchwatch-tauri/src/bridge_runtime.rs`).

## Component B: Bazzite scanner
```
 scanner-cli (userspace, one-shot) -> scanner-core (checks, SystemInspector, scans.db)
 Deep scan: GUI -> [Flatpak] scanner-broker (session D-Bus org.snitchwatch.Scanner)
                -> pkexec -> scanner-privileged (root, one-shot, watchdog) -> JSON on stdout
            GUI -> [native] pkexec scanner-privileged directly (kirigami src/scanner.rs:98)
```
No persistent privileged process (invariant in `crates/scanner-privileged/src/lib.rs`).

## How A and B relate
Only the Kirigami GUI links scanner-broker (`crates/snitchwatch-kirigami/Cargo.toml`) to trigger a deep scan from `qml/ScannerPage.qml`. Optional future one-way signal B reading A's connection log: `scanner-core` outbound check is a deferred stub (`crates/scanner-core/src/checks/outbound.rs`).

## Trust boundaries
| Boundary | Guard |
|---|---|
| daemon -> bridge | Unix socket perms (system); TCP loopback in per-user mode |
| GUI -> bridge | 0700 dir / 0600 socket, token as first WS frame, `SO_PEERCRED` uid recorded (`ws_server.rs:161`) |
| Flatpak -> host | `--filesystem=xdg-run/snitchwatch`, broker takes no exe/args/env |
| user -> root scan | polkit `packaging/polkit/org.snitchwatch.scanner.policy` |

## Data flow (prompt)
daemon `AskRule` -> `grpc_server.rs:357` -> deferred answer slot (`deferred_answers.rs`) + `ServerMessage::InsertConnectionRows` broadcast -> GUI row -> user verdict `ClientMessage` -> `translator::upstream::apply` (`translator/upstream.rs:47`) -> rule reply to daemon. Unanswered prompts time out (`ANSWER_TIMEOUT`).

See: bridge.md, gui.md, scanner.md, data.md, dependencies.md.
