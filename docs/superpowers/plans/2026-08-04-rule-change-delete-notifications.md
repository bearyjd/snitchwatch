# Plan: Wire the outbound Notifications stream (CHANGE_RULE / DELETE_RULE)

## Summary

The Rules page's enable/disable/delete controls are wired end to end *up to the
bridge* and then stop: `UpstreamEffect::{UpdateRule, DeleteRule, AddRule}` are
produced and then dropped on the floor, because the bridge's outbound
`Notifications` stream parks on `std::future::pending()`. This plan adds the
missing leg — a notification channel into `UiService`, a JSON→proto `Rule`
conversion, and the effect-to-`Notification` translation — so a rule toggle or
delete actually reaches `opensnitchd`.

## User Story

As someone managing my firewall rules,
I want the Rules page's enable/disable/delete buttons to actually change the daemon's rules,
So that the UI reflects reality instead of silently discarding my clicks.

## Problem → Solution

**Current:** UI click → `ClientMessage` → `UpstreamEffect::UpdateRule` → logged by the
catch-all `Ok(effect) => info!(?effect, "applied upstream effect")` arm → **nothing**.
The daemon never hears about it. The rule row does not change, because the row only
ever reflects the daemon's next push.

**Desired:** UI click → `ClientMessage` → `UpstreamEffect` → `Notification{type:
CHANGE_RULE|DELETE_RULE, rules: [...]}` → daemon `Replace`/`Delete` → daemon pushes
updated rules → UI row updates.

## Metadata

- **Complexity**: Medium (bridge + test double + tests; no new dependencies)
- **Source PRD**: N/A — standalone gap called out in `HANDOFF.md`
- **Estimated Files**: 6 changed, 1 new test

---

## UX Design

### Before

```
┌───────────────────────────────────────────────┐
│ Rules                                         │
│  ▸ 899-firefox-allow-out      [x] Enabled  🗑 │
│                                               │
│  user clicks [x] → nothing happens.           │
│  No error, no revert, no change. The toggle   │
│  snaps back on the daemon's next push.        │
└───────────────────────────────────────────────┘
```

### After

```
┌───────────────────────────────────────────────┐
│ Rules                                         │
│  ▸ 899-firefox-allow-out      [ ] Disabled 🗑 │
│                                               │
│  user clicks [x] → CHANGE_RULE → daemon       │
│  Replace() → daemon pushes rules → row shows  │
│  Disabled. Delete removes the row.            │
└───────────────────────────────────────────────┘
```

### Interaction Changes

| Touchpoint | Before | After | Notes |
|---|---|---|---|
| Rules row enable toggle | No-op | Rule enabled/disabled in the daemon | Persists to disk only when `duration == "always"` — see GOTCHA 3 |
| Rules row delete | No-op | Rule deleted from the daemon | `rules.Delete(name)` always removes from disk |
| Failure feedback | None | Daemon `NotificationReply{code: ERROR}` logged | Surfacing it in the UI is explicitly NOT in this plan |

---

## Mandatory Reading

| Priority | File | Lines | Why |
|---|---|---|---|
| P0 | `crates/snitchwatch-bridge/src/grpc_server.rs` | 510–552 | The `pending()` gap being replaced |
| P0 | `vendor/opensnitch/daemon/ui/notifications.go` | 87–138 | What each action handler actually requires |
| P0 | `vendor/opensnitch/daemon/ui/notifications.go` | 371–412 | Receive loop — incl. the `Type <= NONE` kill switch |
| P0 | `crates/snitchwatch-bridge/src/grpc_server.rs` | 31–58 | `rule_to_wire`/`operator_to_wire` — invert these |
| P1 | `crates/snitchwatch-bridge/src/translator/upstream.rs` | 23–80 | `UpstreamEffect` variants + `apply` |
| P1 | `crates/snitchwatch-bridge-cli/src/lib.rs` | 395–456 | Effect-handling loop; the catch-all at **455** (`Ok(effect) => info!(...)`) is what currently swallows the rule effects — new arms go above it |
| P1 | `crates/snitchwatch-kirigami/src/rules_model.rs` | 212–227 | Confirms the UI already sends the full rule |
| P1 | `tests/mock_opensnitchd/src/lib.rs` | 199–218 | `open_notifications` — must be extended |
| P1 | `tests/mock_opensnitchd/src/lib.rs` | 221–330 | `validate_rule_shape` — daemon-fidelity validator to reuse |
| P2 | `vendor/opensnitch/proto/ui.proto` | 165–200, 265–295 | `Action` enum, `Notification`, `NotificationReply` |
| P2 | `crates/snitchwatch-bridge/src/grpc_server/tests.rs` | 298–390 | Test pattern for a bridge↔daemon round trip |

## External Documentation

No external research needed — the authoritative contract is the vendored
`opensnitchd` source, read directly above.

---

## Patterns to Mirror

### PROTO_TO_WIRE (invert this for `rule_from_wire`)
```rust
// SOURCE: crates/snitchwatch-bridge/src/grpc_server.rs:34-58
fn rule_to_wire(rule: &Rule) -> serde_json::Value {
    serde_json::json!({
        "name": rule.name,
        "enabled": rule.enabled,
        "action": rule.action,
        "duration": rule.duration,
        "description": rule.description,
        "operator": rule.operator.as_ref().map(operator_to_wire).unwrap_or(serde_json::Value::Null),
    })
}
```

### EFFECT_HANDLING (insertion point + logging style)
```rust
// SOURCE: crates/snitchwatch-bridge-cli/src/lib.rs:431-452
Ok(UpstreamEffect::VerdictApplied { row_id, .. }) => {
    // ...
    if let Err(e) = snapshot_tx.send(ServerMessage::UpdateConnectionRows { rows: vec![row] }) {
        warn!(error = %e, "verdict update broadcast failed");
    }
    info!(%row_id, "applied verdict and broadcast row update");
}
Ok(effect) => info!(?effect, "applied upstream effect"),
Err(e) => error!(error = %e, "upstream apply failed"),
```

### SERVICE_STATE (how to add the sender to `UiService`)
```rust
// SOURCE: crates/snitchwatch-bridge/src/grpc_server.rs:92-99
/// True while the user has paused interactive filtering (tray
/// "Pause filtering"). Unlike `liveness`/`block_generation`, this must
/// be *writable* from outside `UiService` ... so it's a genuine
/// constructor parameter, not internal-only state.
filtering_paused: Arc<AtomicBool>,
```

### STREAM_HANDLER (the shape being replaced)
```rust
// SOURCE: crates/snitchwatch-bridge/src/grpc_server.rs:542-547
let outbound = async_stream::try_stream! {
    // Hold the stream open with no commands until M3+ wires up
    // config-push from the GUI side.
    let () = std::future::pending().await;
    yield Notification::default();
};
```

### TEST_STRUCTURE (bridge↔daemon round trip)
```rust
// SOURCE: crates/snitchwatch-bridge/src/grpc_server/tests.rs:298-310
#[tokio::test]
async fn persistent_allow_verdict_broadcasts_rule_for_live_clients() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    // ... drive the service, assert on the broadcast
}
```

### MOCK_VALIDATION (reuse, do not reinvent)
```rust
// SOURCE: tests/mock_opensnitchd/src/lib.rs:252-256
fn validate_rule_shape(rule: &Rule) -> Result<(), MockError> {
    validate_rule_name(&rule.name)?;
    // ... operator None check, Compile() check, duration check
}
```

---

## Files to Change

| File | Action | Justification |
|---|---|---|
| `crates/snitchwatch-bridge/src/grpc_server.rs` | UPDATE | Add notification sender to `UiService`; replace `pending()` with a real relay loop; add `rule_from_wire`/`operator_from_wire` |
| `crates/snitchwatch-bridge/src/translator/upstream.rs` | UPDATE | No variant changes needed; add unit tests for wire round-trip if conversion lands here instead |
| `crates/snitchwatch-bridge-cli/src/lib.rs` | UPDATE | Translate `UpdateRule`/`DeleteRule`/`AddRule` effects into `Notification`s and send |
| `tests/mock_opensnitchd/src/lib.rs` | UPDATE | `open_notifications` must yield the full `Notification`, not just `n.id`; expose `validate_rule_shape` for the notification path |
| `tests/bridge_protocol_test.rs` | UPDATE | Add the end-to-end round trip |
| `HANDOFF.md` | UPDATE | Remove the "do not claim rule editing works" caveat once it does |

## NOT Building

- **Surfacing daemon `NotificationReply{ERROR}` in the UI.** Replies stay logged.
  A real error-toast path is its own change.
- **`AddRule` from the UI.** No UI affordance creates a rule directly today
  (rules come from verdicts and blocklists). Wire the effect for completeness but
  do not add UI.
- **Optimistic UI updates.** Rows keep reflecting the daemon's next push, per the
  existing "no local optimistic mutation" convention in `rules_model.rs`.
- **The remaining `Action` variants** (`ENABLE_INTERCEPTION`, `CHANGE_CONFIG`,
  `LOG_LEVEL`, `TASK_*`, firewall actions). Out of scope.
- **Multi-daemon fan-out.** One connected daemon is assumed, matching every other
  handler in `grpc_server.rs`.

---

## Step-by-Step Tasks

### Task 1: `rule_from_wire` / `operator_from_wire`
- **ACTION**: Add the inverse of `rule_to_wire` in `grpc_server.rs`.
- **IMPLEMENT**: `fn rule_from_wire(v: &serde_json::Value) -> Result<Rule, String>`,
  parsing `name`/`enabled`/`action`/`duration`/`description`/`operator`, recursing
  for nested `operands` (mirror `operator_to_wire`'s two branches: leaf vs list).
- **MIRROR**: PROTO_TO_WIRE.
- **IMPORTS**: `snitchwatch_proto::protocol::{Rule, Operator}`.
- **GOTCHA**: **This is the issue #14 failure mode.** A `Rule` whose `operator` is
  `None` — or whose `operator.type` is unknown, or whose `regexp` data does not
  compile — is rejected by the daemon (`rule.Deserialize`, then
  `Operator.Compile()`). Return `Err`, never a rule with `operator: None`.
- **VALIDATE**: Unit test round-tripping `rule_to_wire(rule_from_wire(json)) == json`
  for a leaf operator and a `list` operator.

### Task 2: Notification channel on `UiService`
- **ACTION**: Add `notification_rx` plumbing so an external producer can push
  `Notification`s into the open stream.
- **IMPLEMENT**: Hold a `broadcast::Sender<Notification>` (broadcast, not mpsc, so a
  reconnecting daemon resubscribes cleanly) as a `UiService` field + constructor
  param, with a public accessor for the orchestrator.
- **MIRROR**: SERVICE_STATE (`filtering_paused` is the precedent for
  externally-writable service state).
- **IMPORTS**: `tokio::sync::broadcast`.
- **GOTCHA**: Every existing `UiService::new` call site must be updated — check
  `grpc_server/tests.rs` and `bridge-cli` both.
- **VALIDATE**: `cargo check -p snitchwatch-bridge` compiles; existing tests still pass.

### Task 3: Replace `pending()` with the relay loop
- **ACTION**: Rewrite the `outbound` stream at `grpc_server.rs:542-547`.
- **IMPLEMENT**: `async_stream::try_stream!` looping on `rx.recv().await`, yielding
  each `Notification`; on `RecvError::Lagged` log and continue; on `Closed` end the
  stream.
- **MIRROR**: STREAM_HANDLER.
- **GOTCHA**: **Never yield `Notification::default()`.** Its `type` is `NONE` (0), and
  the daemon treats `ntf.Type <= Action_NONE` as "server ordered to close
  notifications" and tears down the stream
  (`notifications.go:405-408`). The current placeholder would do exactly this if it
  were ever reached.
- **GOTCHA**: The daemon sends a HELLO `NotificationReply{Id: 0, Code: OK}` on connect
  (`notifications.go:376-383`) before any real notification. Do not treat reply id 0
  as a response to a sent notification.
- **VALIDATE**: New integration test (Task 6) observes a notification arriving.

### Task 4: Effect → Notification translation
- **ACTION**: Handle the three rule effects in `bridge-cli`'s effect loop.
- **IMPLEMENT**:
  - `UpdateRule { rule, .. }` → `Action::ChangeRule`, `rules: vec![rule_from_wire(rule)?]`
  - `DeleteRule { rule_id }` → `Action::DeleteRule`, `rules: vec![Rule { name: rule_id, ..Default::default() }]`
  - `AddRule { rule }` → `Action::ChangeRule` (the daemon has no ADD; `Replace` creates)
  - Assign a monotonically increasing non-zero `id` from an `AtomicU64`.
- **MIRROR**: EFFECT_HANDLING (place these arms *before* the `Ok(effect)` catch-all,
  which currently swallows them).
- **IMPORTS**: `snitchwatch_proto::protocol::{Notification, Action, Rule}`.
- **GOTCHA**: `DELETE_RULE` only reads `rul.Name` (`notifications.go:132`), so a
  name-only `Rule` is correct there and must NOT be routed through
  `rule_from_wire`'s operator validation.
- **GOTCHA**: A `rule_from_wire` error must be logged and dropped, never sent — a
  malformed rule reaching the daemon is the issue #14 regression.
- **VALIDATE**: Unit test asserting each effect maps to the expected `Action` + rule payload.

### Task 5: Extend `MockOpensnitchd`
- **ACTION**: Make the mock able to assert on notifications.
- **IMPLEMENT**: Change `open_notifications` to yield `Notification` instead of `u64`
  (`mpsc::Receiver<Notification>`); apply `validate_rule_shape` to every rule in a
  `CHANGE_RULE`/`ENABLE_RULE`/`DISABLE_RULE` notification and surface a rejection the
  way `ask_rule` does.
- **MIRROR**: MOCK_VALIDATION.
- **GOTCHA**: Line 211 currently sends `n.id` and discards the rest — the whole point
  of this task. Update the single existing caller.
- **VALIDATE**: `cargo test -p snitchwatch-bridge` passes; the mock rejects a
  known-bad rule.

### Task 6: End-to-end round-trip test
- **ACTION**: Add a test to `tests/bridge_protocol_test.rs`.
- **IMPLEMENT**: Boot the bridge, connect `MockOpensnitchd`, open the notifications
  stream, send `ClientMessage::UpdateRule` with a full valid rule over the inbound
  pump, assert a `CHANGE_RULE` notification arrives whose rule survives
  `validate_rule_shape`. Repeat for `DeleteRule` → `DELETE_RULE` with the right name.
- **MIRROR**: TEST_STRUCTURE, and the `ask_rule_round_trip_*` pattern named in
  `CLAUDE.md` as the load-bearing template.
- **GOTCHA**: The notification stream must be opened *before* the effect is sent, or
  the broadcast receiver misses it.
- **VALIDATE**: **Sabotage check — mandatory.** Revert Task 3's loop to `pending()`
  and confirm this test FAILS. A test that passes either way is worthless (see the
  2026-08-04 vacuous-assertion incident in `HANDOFF.md`).

### Task 7: Update `HANDOFF.md`
- **ACTION**: Replace the "Do not claim rule editing works" caveat with what now works.
- **IMPLEMENT**: State that CHANGE_RULE/DELETE_RULE are wired and tested against the
  mock, and that **live-hardware confirmation is still open**.
- **VALIDATE**: No stale claim remains — `rg -n "pending\(\)" HANDOFF.md`.

---

## Testing Strategy

### Unit Tests

| Test | Input | Expected Output | Edge Case? |
|---|---|---|---|
| `rule_from_wire` leaf operator | `{"name":"r","operator":{"type":"simple","operand":"dest.host",...}}` | `Rule` with populated `Operator` | No |
| `rule_from_wire` list operator | operator with `operands: [...]` | nested `Operator.list` | Yes |
| `rule_from_wire` rejects null operator | `{"operator": null}` | `Err` | Yes — issue #14 class |
| `rule_from_wire` rejects unknown type | `{"operator":{"type":"bogus"}}` | `Err` | Yes |
| wire round trip | `rule_to_wire(r)` → `rule_from_wire` | equals `r` | No |
| effect → notification | `UpstreamEffect::UpdateRule` | `Action::ChangeRule` + 1 rule | No |
| delete effect → notification | `UpstreamEffect::DeleteRule{id}` | `Action::DeleteRule`, `rules[0].name == id` | No |
| notification id | two effects | strictly increasing, both non-zero | Yes |

### Edge Cases Checklist
- [ ] Rule with `operator: null` → dropped with an error log, never sent
- [ ] Rule with an uncompilable `regexp` operator → dropped
- [ ] Delete of an unknown rule name → sent; daemon errors; reply logged
- [ ] Notification sent while no daemon is connected → dropped, no panic
- [ ] Daemon reconnects → new stream receives subsequent notifications
- [ ] Broadcast lag → logged, stream survives
- [ ] `Notification.type` is never `NONE`

---

## Validation Commands

### Static Analysis
```bash
CCACHE_DISABLE=1 cargo clippy --all-targets -- -D warnings
```
EXPECT: zero warnings

### Unit + Integration Tests
```bash
CCACHE_DISABLE=1 cargo test -p snitchwatch-bridge -p snitchwatch-bridge-cli
```
EXPECT: all pass, including the new round trip

### Full Suite
```bash
CCACHE_DISABLE=1 cargo test
CCACHE_DISABLE=1 QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic \
  cargo test -p snitchwatch-kirigami
just qml-test
```
EXPECT: no regressions. Run Kirigami cargo jobs **serially** — concurrent cargo
invocations poison the shared cxx-qt build state (`Conflicting include_prefixes`);
recover with `cargo clean -p cxx-qt -p cxx-qt-lib -p snitchwatch-kirigami`.

### Format
```bash
cargo fmt --all --check && git diff --check
```

### Manual Validation (hardware — deferred)
- [ ] Toggle a rule in the GUI against a live `opensnitchd`; confirm the daemon logs
      `[notification] change rule:` and the row flips
- [ ] Delete a rule; confirm `[notification] delete rule:` and the row disappears
- [ ] Restart the daemon; confirm an `always`-duration toggle persisted and a
      non-`always` one did not (expected — see GOTCHA 3)

---

## Acceptance Criteria
- [ ] All 7 tasks complete
- [ ] `rule_from_wire` rejects every malformed-operator class the mock validates
- [ ] Round-trip test passes AND fails when Task 3 is reverted (sabotage-verified)
- [ ] No clippy warnings, no fmt diff
- [ ] `HANDOFF.md` no longer claims rule editing is unimplemented

## Completion Checklist
- [ ] Follows `rule_to_wire`'s existing conversion style
- [ ] Logging matches the `warn!(error = %e, "...")` / `info!(%id, "...")` house style
- [ ] Tests follow `ask_rule_round_trip_*`
- [ ] No hardcoded notification ids
- [ ] No optimistic UI mutation introduced
- [ ] Every new assertion sabotage-verified

## Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Malformed rule reaches daemon → silently ignored, or default action applied | Medium | High — repeats issue #14 | `rule_from_wire` validates; mock's `validate_rule_shape` asserts in tests |
| Accidentally sending `type: NONE` kills the daemon stream | Medium | High — silent loss of all notifications | Never construct `Notification::default()`; assert `type != NONE` in tests |
| Non-`always` rules don't persist across daemon restart | High | Medium — looks like a bug to users | Document; consider a UI hint later (out of scope) |
| Broadcast receiver missed because stream opened late | Medium | Medium — flaky test | Open the stream before sending in tests; broadcast (not mpsc) so reconnects resubscribe |
| `UiService::new` signature change breaks call sites | High | Low — compiler catches | Update all sites in Task 2 |

## Notes

- **The UI side needs no changes.** `rules_model.rs:212-227` already sends
  `UpdateRule` with the full toggled rule JSON (via `store.toggled_rule_json`) and
  `DeleteRule` with the name. This was the main open question during exploration and
  it resolved in the implementation's favour.
- **Why `CHANGE_RULE` rather than `ENABLE_RULE`/`DISABLE_RULE`:** all three end in
  `c.rules.Replace(r, ...)`; `ENABLE`/`DISABLE` merely force `r.Enabled` before the
  replace. Since the UI already sends the desired `enabled` value in the rule,
  `CHANGE_RULE` expresses it in one action and avoids a branch. Revisit only if the
  daemon's logging distinction turns out to matter.
- **GOTCHA 3 in full:** `Replace(r, r.Duration == rule.Always)` — the second argument
  is "save to disk". A toggle on a `once`/`5m`/`until restart` rule changes the
  in-memory rule only.
- **What this plan missed, found during review:** `Replace` is *wholesale*, so any
  field the wire shape drops is a field a toggle silently clears on the daemon. The
  plan's "Files to Change" table did not include
  `snitchwatch-kirigami/src/rules/row_store.rs`, whose `Rule` struct carried neither
  `precedence` nor `nolog` — so the first implementation reset both on every toggle.
  `precedence` decides whether a rule is evaluated ahead of others, meaning an
  "enable/disable" click could quietly change which rule wins for unrelated traffic.
  Fixed by carrying both fields end to end (`rule_to_wire` -> store -> `rule_from_wire`)
  with a regression test on each half. **Lesson for the next plan touching this path:
  enumerate every proto field and decide explicitly whether it round-trips**, rather
  than inferring the wire shape from what the UI happens to display.
