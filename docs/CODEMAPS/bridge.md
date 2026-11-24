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
| `history/` | SQLite connection history, off by default. Capture includes daemon events and accepted prompt outcomes. The enable control is planned. |
| `analysis_export/` | redacted cache-only export |
| `sqlite_file.rs`, `state_file.rs` | owner-only file helpers |

## Notes
- `history/dormant_tests.rs` limits production access to history. The bridge CLI starts capture from existing consent, and `grpc_server.rs` passes decided pings and accepted prompts to it. `history_off.rs` verifies that the off path creates neither a writer thread nor a queue.
- An accepted prompt holds an arrival pin until its outcome commits, transfers to a loss tally, or is discarded after a generation change. The version 4 schema adds the expiry debt column to `meta` (v1 to v3 files migrate in one bounded transaction; a build that knows only v3 refuses a v4 file by its version). The version 3 `history_events` view flags plausible duplicate prompt and ping records. The daemon protocol lacks socket-lifetime identity, so the view retains both records. In history, `answered_by = gui` means “answered in GUI (may be silent mode).”
- **Retention (PR 8, `history/retention.rs`).** One deletion path removes up to `rows_per_txn` rows per transaction, with the snapshots they leave unreferenced and the `evicted` gap row in the same transaction. Order: age expiry (a pass every 10 minutes, at most a tenth of the rows, skipped once after a clock jump, giving way to shutdown or after one second; and, whatever the clock says, within a bucket of one eighth of the events that refills over 24 hours of the bridge's own monotonic uptime, `expiry_budget.rs`: schema v4 keeps its debt in `meta.expiry_debt_milli`, written in the deleting transaction, so a clock stepped forward cannot erase history, a restart does not refill it, and a store left idle expires over days, not in one pass); under pressure, each process over its byte share loses its own oldest rows first (events by `measure.rs`, plus snapshots only it uses); then the oldest rows of all. Admission no longer refuses for held bytes: its ledger is seeded from the database at start and released as rows are deleted, but it is not a gate. Eviction goes down to a low-water mark (a twentieth of the file under high water) so one pass serves many batches; tier 2 scans per-process bytes only when a cached per-process tally says some process could be over its share (`retention_room.rs`); chunks go on while they remove rows, up to 128. When that cannot get under the high-water mark, recording pauses (500 ms, doubling to 60 s) and lost batches become plain `write-failed` gaps; `CaptureStats::cap_paused` and `lost_cap` hold the cap label, which is not stored. Capture losses (queue full, pin refused, admission refused) each hold one pin (`LossCounter`) until PR 7 flushes a gap row.
