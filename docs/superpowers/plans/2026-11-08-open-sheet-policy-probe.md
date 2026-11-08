# Plan: probe for the open rule-editor sheet following the daemon policy

Date: 2026-11-08. Status: PLANNED (follow-up to #342, merged as e76c03e5). Not started.

## Why
#342 makes an open rule-editor sheet follow the live daemon policy: a task
(`rule_editor_policy_follow.rs`) re-reads `BridgeHandles::daemon_policy()` on
every `link_wake` change and on the Qt thread sets `refuseSourceNetwork`;
`RuleEditorSheet.qml` then re-runs `controller.check(...)` through
`Qt.callLater`. Codex's last review (MEDIUM, test coverage only): nothing
fails if the follower or the QML handler is deleted. The existing probe
(`crates/snitchwatch-kirigami/tests/source_network_gate_qml.rs`, 271 lines)
sets the policy while the sheet is closed or before it opens.

## Two layers (each needs its own test)
1. **QML handler** (`onRefuseSourceNetworkChanged` -> `Qt.callLater` ->
   `controller.check`). Headless the controller has no bridge runtime, so the
   probe may set `controller.refuseSourceNetwork` itself, as it already does.
   New probe steps, sheet kept OPEN throughout:
   - open a new rule, add a condition `source.network` / `network` /
     `10.0.0.0/8` plus a valid process condition; assert no problems and Save
     enabled;
   - set `controller.refuseSourceNetwork = true` with the sheet still open;
     wait one event-loop turn (a zero-interval `Timer`, because the re-check is
     deferred); assert `sheet.checked.problems` contains the "opensnitchd 1.9
     and newer" refusal and `editorSave` is disabled;
   - set it back to `false`; wait a turn; assert the problems are gone and Save
     is enabled again;
   - close the sheet, flip the property, assert nothing throws (the handler
     guards `sheet.visible`).
2. **The follower** (`spawn_policy_follow`). It takes a `QtThread`, so it can't
   be driven without Qt. Refactor the loop body into a function generic over
   the link receiver, a `read_policy` closure and an `apply` closure (no Qt
   types), keep `spawn_policy_follow` as a thin wrapper, and unit-test the
   function with a fake `watch` channel: (a) one apply at start, (b) one apply
   per bump reading the value at apply time (change the fake policy between
   bump and apply and assert the NEW value is applied), (c) the loop ends when
   `apply` reports the target gone, (d) no apply without a bump.

## Mutation checks (run as part of the PR, results in its body)
- delete the `onRefuseSourceNetworkChanged` handler -> layer 1 must fail;
- replace `Qt.callLater(...)` by a direct call -> must still pass (behaviour
  equal; only re-entrancy differs) unless a re-entrancy probe is added;
- make the follower read the policy off the Qt thread and queue the value ->
  layer 2 (b) must fail;
- never bump `link_wake` from `observe` -> the existing
  `a_changed_policy_wakes_link_watchers` must fail.

## Constraints
- QML integration tests SEGFAULT in the author sandbox (any tree); the CI
  `kirigami` job (~15-20 min per push) is the only way to run layer 1. Expect
  one or two CI iterations; keep the probe small.
- New QML files must be registered in `build.rs` and
  `tests/qml_guard_support/sources.rs`; a new probe needs no registration:
  embed it as an inline string and `load_data` it under a nominal `qrc:/` URL,
  as `source_network_gate_qml.rs` does.
- `rules_model.rs` sits at the 800-line cap; put new Rust helpers in new files.

## Also fold in (optional, from the #342 reviews)
- name the deferred closure (`function recheck()` + `Qt.callLater(sheet.recheck)`)
  so rapid flips coalesce, and guard `sheet && sheet.controller`;
- doc link in `editor_profile.rs` points at a `cfg(test)` item;
- the policy re-send shares the 32-slot reply queue in `ws_server.rs`
  (security review LOW): send it from the outbound task instead.
