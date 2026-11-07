<!-- Generated: 2026-11-06 | Files scanned: 40 | Token estimate: ~950 -->
# GUI

## snitchwatch-kirigami (target shell; excluded from `default-members`)
Qt6/QML + Kirigami via cxx-qt. 428 .rs, 102 `qml/*.qml` (~15.9k lines), 152 `tests/*` files. QML module `com.snitchwatch.shell` is declared in `crates/snitchwatch-kirigami/build.rs:17`; `qml/main.qml` is loaded from `src/main.rs`.

### Startup (`src/main.rs`)
geo validation worker check -> `logging::init` -> `panic_hook::install(paths::crash_log_path())` -> `bridge_runtime::ensure_started()` (skipped when `demo::enabled()`) -> QQmlApplicationEngine loads `main.qml` -> on exit `learned::flush_and_wait`.

### State flow
```
bridge WS (Unix sock) --bridge_runtime.rs--> broadcast<ServerMessage> --bridge_dispatch/bridge_feed--> per-model tasks --> Qt thread --> QML
QML action --> model/controller (qinvokable) --> BridgeHandles::send ClientMessage --> bridge
```
- `bridge_runtime.rs`: client loop `client_loop` :372, `connect_and_relay` :410, `await_authentication_ack` :508; token path `resolve_token_path` :290; `handles()` :688. Env: `SNITCHWATCH_WS_SOCKET`, `SNITCHWATCH_SYSTEM_BRIDGE`, `SNITCHWATCH_WS_TOKEN_PATH`.
- `bridge_feed.rs:145` `BridgeFeed`: link status (`ok/statusText/linkState`) for `main.qml` InlineMessage. Each model filters messages before queueing to Qt.

### Rust <-> QML objects (cxx-qt `#[qml_element]`)
BridgeFeed, ConnectionsModel, RulesModel, TrafficModel, BlocklistsModel(+EntriesModel), ProfilesModel, CuratedDefaultsModel, DaemonHealthModel, DenyStatusModel, GeoModel, AboutModel, PendingInsight, PromptSlotStatus, SettingsController, TrayController, NotificationController, ScannerController, WizardController, RuleEditorController, MakeRuleController, RulesIoController, LearningReviewController, AnalysisExportController, NativeClipboard, AppInfo. Pure Qt-free cores live beside each wrapper (`connections/`, `rules/`, `geo/`, `learned/`) so tests need no Qt.

### QML tree (`qml/`)
`main.qml` -> `GlobalDrawer`, `StackNav` + `pageStack`; pages: Connections (`ConnectionsPage`, `ConnectionList/Row/DetailPage`, `InspectorPanel`), Rules (`RulesPage`, `RuleEditorSheet`, `RulesImportSheet`, `RuleSimulatorSheet`), Blocklists, Profiles, CuratedGroups/RecommendedRules, Traffic, Geo (`GeoPage`, `Map*`), Diagnostics/DaemonHealth, Scanner, Learning (`LearningReviewPage`, `LearningBanner`), SilentMode, Onboarding, About. Alerts: `AlertHost`, `AlertWindow`, `PendingDecisionSheet`. Primitives: `Rounded*`, `Guarded*`, `SelectableText`, `RowCopyMenu`, `Theme`.

### Tray and notifications
`TrayController` (`src/tray_controller.rs`) feeds `TrayMenu.qml` (`Qt.labs.platform.SystemTrayIcon`); `src/tray.rs` derives tooltip/menu label from bridge `TrayState`. Desktop notifications: `notifier.rs`, `notification_controller.rs`, `notification_signals.rs`.

### Silent and learning mode
`settings.rs:20` (kirigami) `SilentMode {Off, Allow, Deny}` persisted in settings.json. `silent_mode.rs` answers new rows once-only (`SetVerdict ... once`), marks them `needsRule`. Learning window records allowed connections via `learned/` (`learned/shared.rs:77` live recorder + writer thread, `store.rs` file w/ advisory lock and 30-day discard `learned/store.rs:229`, `aggregate.rs` bounds).

### Debug flags
`SNITCHWATCH_TEXT_TRACE=1` (exactly "1") sets `AppInfo.textTrace` (`src/bridge_bindings.rs:44`) -> `qml/TextTrace.qml` logs on-screen text changes. `SNITCHWATCH_DEMO=1` fake rows, no bridge (`src/demo.rs:14`); `SNITCHWATCH_SCREENSHOT_DIR` (`demo.rs:16`); `SNITCHWATCH_LOG` file logging (`logging.rs`).

## Legacy: snitchwatch-tauri + web/
Tauri 2 shell (12 .rs): embeds the bridge in-process (`src/bridge_runtime.rs`), proxies a loopback TCP port to the bridge Unix socket for WebKitGTK (`src/loopback_proxy.rs`), plus tray/notifier/wizard. Frontend: `web/` (22 files, `web/js/*.js`, vendored Objective Development GPL-2.0-only code, rebrand via `web/rebrand.sh`). Kept only during the Kirigami migration; Kirigami ported `tray.rs` logic from it.
