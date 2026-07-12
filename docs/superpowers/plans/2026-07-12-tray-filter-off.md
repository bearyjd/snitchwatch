# Tray state: FilterOff (pause/resume filtering)

**Date:** 2026-07-12
**Status: DONE.** All layers landed as designed below: `ClientMessage::
SetFilteringPaused`, `UiService`'s new `filtering_paused` constructor
parameter + `ask_rule` auto-allow path, the inbound-pump toggle handler,
and the Kirigami shell wiring (`TrayController::toggleFiltering`, the
`main.qml` tray menu item, `build_set_filtering_paused_json`). Full
workspace test suite green (`cargo test --workspace`, 0 failures across
every crate); `snitchwatch-kirigami`'s 247 tests include a passing
`smoke.rs` main.qml load, confirming the new tray menu item didn't break
QML parsing. All items in `.agent_native/agent_roadmap.md` item 9 are now
resolved — no unbuilt tray-state variants remain.

**Follow-up to:** `.agent_native/agent_roadmap.md` item 9's last unbuilt
variant. Owner decision (this session): pausing keeps `opensnitchd`'s
fail-closed `DefaultAction: deny` untouched — the bridge itself
auto-resolves every `AskRule` as `Allow-Once` while paused, no prompt shown.
This preserves the "genuine bridge outage still fails closed" property
Phase 2's whole packaging effort protects; only the *bridge's own* decision
policy changes while paused, not the daemon's own safety net.

## Why this is bigger than DaemonDown/RecentBlock

Those two are purely reactive — the bridge observes its own state
(ping recency, a Deny verdict) and republishes tray state. `FilterOff` is
**user-initiated**: a real control path from the tray menu click through to
bridge state is needed, and it didn't exist at all — `TrayController`'s
`menuLabel` already computed `pause_filtering`/`resume_filtering` tokens,
but no tray menu item ever displayed them or had a click handler wired to
anything.

## Design

- **New `ClientMessage::SetFilteringPaused { paused: bool }`** — same
  `#[serde(tag = "action", rename_all = "camelCase")]` shape every other
  client message uses, so it can travel over the existing WS protocol *and*
  the in-process `BridgeFeed::sendClientJson` path the Kirigami shell
  already uses for `SetVerdict` etc. No new transport.
- **`UiService` gains `filtering_paused: Arc<AtomicBool>`**, this time as a
  genuine `new()` constructor parameter (unlike `last_ping`/
  `block_generation`, which stayed internal-only): the flag must be
  *writable* from outside (the inbound pump) and *readable* from inside
  (`ask_rule`), the same shape `tray_pub`/`cache` already have. All existing
  `UiService::new` call sites (tests + `snitchwatch-bridge-cli`) get the new
  argument.
- **`ask_rule` checks the flag first.** When paused: build the row via
  `insert_decided` (not `insert_pending`) with `action = "allow"`, broadcast
  it, skip the notice-bus "Pending" desktop notification (nothing for the
  user to review), and return `verdict_to_rule(Verdict::Allow,
  VerdictDuration::Once, ...)` immediately — no oneshot, no wait. When not
  paused, behavior is exactly what it is today.
- **The inbound pump (`snitchwatch-bridge-cli::run`)** special-cases
  `SetFilteringPaused` before the profile-message/`upstream::apply` dispatch
  (mirrors the existing `is_profile_message` short-circuit) — toggles the
  shared flag and publishes `TrayState::FilterOff` (pausing) or
  `cache.resync_tray_state()` (resuming, so it lands on the cache's actual
  Idle/Pending(n), not a hardcoded Idle).
- **Shell wiring (Kirigami only — Tauri is retiring, not worth new
  features):** `TrayController` gains a `toggleFiltering()` qinvokable that
  builds and sends the `SetFilteringPaused` JSON via the existing
  `BridgeFeed`/`bridge_dispatch` path (mirrors `pending_decision.rs`'s
  pattern: a pure, unit-tested `build_set_filtering_paused_json(paused:
  bool) -> String` function plus a thin cxx-qt qinvokable). `main.qml`'s
  tray menu gains one `Labs.MenuItem` bound to `trayController.menuLabel`
  (`"pause_filtering"` → "Pause filtering" / `"resume_filtering"` → "Resume
  filtering"), calling `trayController.toggleFiltering()`.

## Non-goals

- Changing `opensnitchd`'s `DefaultAction` at runtime — explicitly rejected
  by the owner's chosen option.
- Any Tauri-shell UI work — that shell is being retired, not extended.
- Persisting the paused state across a bridge restart — resets to
  unpaused on every bridge start, matching every other in-memory bridge
  state (cache, blocklists store when run in-memory, etc.).

## Testing

- `ws_messages.rs`: JSON round-trip test for the new variant, matching the
  existing per-variant test style.
- `grpc_server.rs`: `ask_rule` auto-allows immediately when paused (no
  pending row, no oneshot wait) and behaves unchanged when not paused —
  both as new unit tests alongside the existing `ask_rule_*` tests.
- Pump-loop toggle: a `snitchwatch-bridge-cli` test that sends
  `SetFilteringPaused` through `inbound_tx` and asserts the tray transitions
  to `FilterOff`/back to `Idle`.
- `pending_decision`-style pure-function test for
  `build_set_filtering_paused_json`, and a `TrayController` QML smoke test
  extension mirroring the existing pattern in
  `tests/scanner_controller_qml.rs` et al.
