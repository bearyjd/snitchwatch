<!-- Generated: 2026-11-06 | Files scanned: 40 | Token estimate: ~910 -->
# GUI

## snitchwatch-kirigami (target shell; excluded from `default-members`)
Qt6/QML + Kirigami (cxx-qt). 428 tracked .rs, 102 `qml/*.qml` (~15.9k lines), 187 tracked files under `tests/` (151 .rs). QML module `com.snitchwatch.shell` is declared in `crates/snitchwatch-kirigami/build.rs:17`; `qml/main.qml` is loaded from `src/main.rs`.

### Startup (`src/main.rs`)
`logging::init` (:31) -> `panic_hook::install(paths::crash_log_path())` (:34) -> `bridge_runtime::ensure_started()` (:61, skipped when `demo::enabled()`) -> QQmlApplicationEngine loads `main.qml` -> on exit `learned::flush_and_wait` (:85).

### State flow
```
bridge WS (Unix sock) --bridge_runtime.rs--> broadcast<ServerMessage> --bridge_dispatch/bridge_feed--> per-model tasks --> Qt thread --> QML
QML action --> model/controller (qinvokable) --> BridgeHandles::send ClientMessage --> bridge
```
- `bridge_runtime.rs`: `client_loop` :372, `connect_and_relay` :410, `await_authentication_ack` :508, `resolve_token_path` :290, `handles()` :688. Env: `SNITCHWATCH_WS_SOCKET`, `SNITCHWATCH_SYSTEM_BRIDGE`, `SNITCHWATCH_WS_TOKEN_PATH`.
- `bridge_feed.rs:145` `BridgeFeedRust`: link status for the `main.qml` InlineMessage. Each model filters messages before queueing to Qt.

### Rust <-> QML objects (cxx-qt)
BridgeFeed, `*Model` types (Connections, Rules, Traffic, Blocklists, Profiles, CuratedDefaults, DaemonHealth, DenyStatus, Geo, About) and `*Controller` types (Settings, Tray, Notification, Scanner, Wizard, RuleEditor, MakeRule, RulesIo, LearningReview, AnalysisExport), plus AppInfo. Qt-free cores sit beside each wrapper (`connections/`, `rules/`, `geo/`, `learned/`) so tests need no Qt.

### QML tree (`qml/`)
`main.qml` -> `GlobalDrawer`, `StackNav`; pages: Connections, Rules (+ editor/import/simulator sheets), Blocklists, Profiles, CuratedGroups, Traffic, Geo (`Map*`), Diagnostics/DaemonHealth, Scanner, Learning, SilentMode, Onboarding, About. Alerts: `AlertHost`, `AlertWindow`, `PendingDecisionSheet`.

### Tray, notifications, modes
`TrayController` (`src/tray_controller.rs`) feeds `TrayMenu.qml`; `src/tray.rs` derives tooltip/menu label from bridge `TrayState`. `settings.rs:20` `SilentMode {Off, Allow, Deny}`; `silent_mode.rs` answers new rows once-only. Learning mode records via `learned/` (`shared.rs:77`; stale file discarded after 30 days, `store.rs:229`).

### Debug flags
`SNITCHWATCH_TEXT_TRACE=1` (exactly "1", `src/bridge_bindings.rs:44`) shows `qml/TextTrace.qml`. `SNITCHWATCH_DEMO=1` fake rows, no bridge (`src/demo.rs:14`); `SNITCHWATCH_SCREENSHOT_DIR` (`demo.rs:16`); `SNITCHWATCH_LOG` file logging (`logging.rs`).

## Legacy: snitchwatch-tauri + web/
Tauri 2 shell (12 .rs): embeds the bridge in-process (`src/bridge_runtime.rs`), proxies loopback TCP to the bridge Unix socket (`src/loopback_proxy.rs`). Frontend `web/` (25 tracked files; vendored GPL-2.0-only code, rebrand via `web/rebrand.sh`). Kept only during the Kirigami migration.
