<!-- Generated: 2026-11-06 | Token estimate: ~810 (o200k_base) -->
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
`ServerMessage` (`ws_messages.rs:49`, tag `action`, camelCase, ~50 variants: Insert/Update/RemoveConnectionRows, SetRules, SetBlocklists, SetProfiles, TrayState, Notice, PromptSlot, DiagnosticsReport ...). `ClientMessage` is macro-generated (`ws_messages.rs:452`) and validated on decode. First frame must equal the token; the bridge replies `Authenticated`. `/stream` is the only route served; every other path is a 404 (static routes and the `web-ui` feature were removed in #379).

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
- `history/mod.rs` says "dormant". `history/dormant_tests.rs` fails if code outside the history owner files names the lifecycle/pins types or modules; if any file outside `history/` names `open_store`, `insert_event`, the history module, `Writer`/`enable(` next to it, or a 3b name (`ledger`, `session`, `durable`, `LossCounter`, `GapCause`, ...); or if a non-owner file inside `history/` names the 3b names. A completeness test fails when `ledger.rs`, `session.rs` or `durable.rs` gain a `pub` item the lists do not cover. PR 3b (durable lifecycle: schema v2 with `gaps`/`sessions`/`meta`, watermark, pin release in the committing transaction, bounded ledger) is built and tested but, like the rest, called from nowhere.
