# r15 group E1: bridge-side file splits (#115, #67.5)

Pure moves, no behaviour change. Public API paths stay identical (re-exports
where a type moves). Test names are compared before and after by leaf name
(`cargo test -- --list`): the module path of a moved test changes, its name
does not.

## 1. `grpc_server/tests.rs` (2276 lines) -> `grpc_server/tests/`
`grpc_server.rs` points `mod tests` at `grpc_server/tests/mod.rs` so the
children resolve to `grpc_server/tests/*.rs`.

| file | content |
|---|---|
| `mod.rs` | shared fixtures: `assert_only_a_free_slot_is_left`, `try_recv_skipping_slot`, `spawn_test_service` |
| `ping_subscribe.rs` | ping and subscribe tests |
| `ask_rule_verdicts.rs` | persistent verdict broadcast, AskRule results, deny-scope notice, concurrent ask ids |
| `recent_block.rs` | recent-block tray TTL tests, process-bound rule wire round trip |
| `pause.rs` | filter-pause auto-allow tests |
| `alerts_lifecycle.rs` | alert store tests, display summary, GUI-lease lifecycle tests |
| `rules_snapshot.rs` | issue #48 rule-list snapshot / HELLO tests |
| `daemon_redial.rs` | `ask_and_resolve`, remembered verdicts, redial and Subscribe ordering |

## 2. `translator/verdict.rs` (1382 lines)
Production code (about 600 lines) stays. The inline `mod tests` (about 775
lines) moves to `translator/verdict/tests/` (`mod.rs` holds
`sample_connection` and the basic rule tests; topical children for domain
scope / PSL, degradation and rule names, process binding, display
sanitising).

## 3. `ws_messages.rs` (947 lines)
Test module declarations (13 lines) and the small wire structs at the tail
(`TrafficEvent`, `ConnectionsStatus`, `AboutInfo`, `StorageStatus`,
`ProfileRuleWire`, `ProfileSummary`, `ReplyTo`) move to `ws_messages/`
submodules, re-exported with `pub use` from `ws_messages`. Serde derives
and field order are untouched.

## 4. `snitchwatch-bridge-cli/src/lib.rs` (848 lines)
Its tests are already in separate files (`lib_tests.rs` etc.), so there is
nothing to move out there. Instead `GrpcEndpoint` + `BridgeConfig` move to
`config.rs` and `RunningBridge` to `running.rs`, both re-exported (`pub use`)
so `snitchwatch_bridge_cli::{GrpcEndpoint, BridgeConfig, RunningBridge}`
are unchanged. `RunningBridge`'s private fields become `pub(crate)` (the
struct literal in `run_with_incoming` and the tests read them).

## Out of scope (also oversized, bridge side)
`release_artifact.rs`, `bridge_protocol_test.rs`, `tests/mock_opensnitchd`,
`packaging_shape.rs`, `curated/manager.rs`.

## Follow-up `test:` commits (#115)
- N1/N8 TCP scenario: B adopts its own snapshot, A commits then closes, B
  must not get A's list back; assert the list-then-withdraw transient.
- Unix HELLO-adopting-nothing test: skipped if it needs a production clock
  hook (recorded in the final report).
