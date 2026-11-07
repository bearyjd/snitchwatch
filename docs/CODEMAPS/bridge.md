<!-- Generated: 2026-11-06 | Files scanned: 48 | Token estimate: ~940 -->
# Bridge (`snitchwatch-bridge` lib, `snitchwatch-bridge-cli` bin)

Lib: 246 .rs files (many `*_tests.rs`). CLI orchestration: `crates/snitchwatch-bridge-cli/src/lib.rs` (`run` :88, `run_with_options` :94, `run_system` :139, `run_with_incoming` :169). `main.rs` is sync and handles `--help/--version` before any socket exists.

## Entry points
| Symbol | Where |
|---|---|
| `UiService` (impl `Ui`: `ping` :263, `ask_rule` :357, `subscribe` :492, `post_alert` :525, `notifications` :581) | `crates/snitchwatch-bridge/src/grpc_server.rs:54` |
| `WsServer::{bind,serve}` | `ws_server.rs:116,151` |
| `Token`, `runtime_dir`, `write_token_file`, `write_system_token_file` | `auth.rs:36,89,98,111` |
| `BridgeConfig::from_env` | `crates/snitchwatch-bridge-cli/src/config.rs:38` |
| `resolve_storage` | `crates/snitchwatch-bridge-cli/src/storage.rs:144` |

## WS protocol
`ServerMessage` (`ws_messages.rs:49`, tag `action`, camelCase, ~50 variants: Insert/Update/RemoveConnectionRows, SetRules, SetBlocklists, SetProfiles, TrayState, Notice, PromptSlot, DiagnosticsReport ...). `ClientMessage` is macro-generated (`ws_messages.rs:452`) and validated on decode. First frame must equal the token; the bridge replies `Authenticated`. Static `/`, `/assets/*` sit behind cargo feature `web-ui` (default on; off in the release tarball).

## Module map (crates/snitchwatch-bridge/src)
| Module | Role |
|---|---|
| `translator/` | daemon <-> Little Snitch protocol mapping |
| `cache/` (`connections.rs:47`, `rules.rs:58`, rule_hits, traffic_*) | in-memory presentation state |
| `rule_policy/`, `rule_io/`, `rule_wire.rs`, `rule_name.rs` | rule validation, export/import, name sanitising |
| `daemon_commands.rs` | outbound commands; `DaemonTransport` :98; allow-list :108 |
| `deferred_answers.rs`, `pause_answers.rs`, `filter_pause.rs`, `prompt_slot.rs` | unanswered prompts, pause mode, one-prompt banner |
| `blocklists/` | subscriptions -> daemon rules; sqlite; trait `RuleSink` (`mod.rs:221`) |
| `profiles/` (`ProfilesManager` `mod.rs:110`, store, network_watcher) | profile rules; own trait `ProfileRuleSink` (`enforcer.rs:53`) |
| `curated/` (data `data/curated-defaults-v1.json`) | opt-in recommended rules |
| `diagnostics/`, `daemon_liveness.rs`, `daemon_watchdog.rs`, `daemon_alerts.rs`, `daemon_config.rs` | daemon health |
| `history/` | sqlite connection history; dormant, not wired |
| `analysis_export/` | redacted cache-only export |
| `sqlite_file.rs`, `state_file.rs` | owner-only file helpers |

## Notes
- `history/mod.rs` says "dormant"; a guard test bans references outside the module (`history/dormant_tests.rs`).
- Curated rules and blocklists install only on the system bridge with persistent storage.
