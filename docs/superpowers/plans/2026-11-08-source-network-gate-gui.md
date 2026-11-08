# source.network gate, GUI half (PR B)

Date: 2026-11-08. Status: DRAFT (defaults taken by the overnight run; owner may change).
Follows `2026-11-08-source-network-gate.md` (PR A, #341). Stacked on `feat/source-network-gate`; rebase onto main once #341 merges.

## Why
PR A makes the bridge refuse `network` + `source.network` for opensnitchd >= 1.9. The GUI still offers the operand,
its editor pre-check uses the 1.8 default, and the simulator says "definitely doesn't match" where a 1.9 daemon would
refuse to load the rule (and where regexp/simple/range + network operands panic the daemon).

## Decision taken by default (owner may change)
Dynamic capability: a new `ServerMessage::DaemonPolicy { refuse_source_network, daemon_version }` sent after
`Authenticated` and again whenever the stored daemon version changes (the daemon can subscribe after the GUI
authenticates). Advertised via a new capability `daemon-policy-v1` so an older bridge is detected and the GUI keeps
today's behaviour. Rejected: a static `advertised()` entry (cannot follow the daemon version).

## Changes
Bridge (default workspace, testable without Qt):
1. `ws_messages.rs`: add the `DaemonPolicy` variant (camelCase, additive). The old-client decode test in
   `bridge_capabilities.rs` (V011 enum) must still pass.
2. `bridge_capabilities.rs`: `DAEMON_POLICY_V1` constant in `advertised()`.
3. `daemon_config/version.rs`: a change notifier on `SharedDaemonVersion` (tokio `watch`), fired by `set`/`set_if_unknown`
   only when the value changes. `ws_server.rs`: pass the handle in `WsHandles`; after `Authenticated`, send the current
   policy, then re-send on every change.
GUI (`snitchwatch-kirigami`, Qt crate):
4. Decode `DaemonPolicy` (`bridge_dispatch.rs` exhaustive match), keep it in connection state, expose it to the editor
   controller and simulator.
5. Editor: use `check_wire_rule_in(.., PolicyContext)` in `editor.rs` (from_wire and check) so `source.network` is refused
   in the editor's words on a 1.9 daemon. Hide the operand in the picker while `refuse_source_network` is set.
6. Simulator: `SimulationInput` gains `refuse_source_network`; `network_operand` returns an outcome with the warning
   "opensnitchd 1.9 and newer refuses to load this rule" for `network` + `source.network`, and a warning
   "opensnitchd panics on this combination" for regexp/simple/range with network operands (replacing the silent
   "definitely doesn't match").
7. Existing enabled `source.network` rules on a 1.9 daemon: show a warning in the rule list/detail (moved here from
   PR A decision 1).

## Tests (failing first)
Bridge: message round trip + old-client decode; policy sent after auth; re-sent on a version change; not re-sent when
unchanged. GUI: editor refuses/accepts per policy; operand hidden; simulator warnings for both cases; QML probe
(`tests/rule_editor_sheet_qml.rs` pattern); old-bridge case (no capability: behaviour unchanged).
Run: `just test-bridge`; `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic cargo test -p snitchwatch-kirigami`
(headless Qt NOT verified in this sandbox; the CI `kirigami` job is the backstop, ~20 min per push).

## Risks
- New `ServerMessage` variant breaks exhaustive matches in the GUI.
- Qt tests may not run headless here; do not claim GUI verification without a run.
- Banned tokens in non-history `.rs`: `identify(`, `rule_identity`, `RuleIdentity`, `IdentityError`, `Qualifiers`, `RuleClass::of`.
