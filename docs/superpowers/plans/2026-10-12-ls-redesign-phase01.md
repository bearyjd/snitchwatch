# Little Snitch-style redesign — Phase 0 + 1 (first r14 PR)

Owner decision 2026-10-12: follow the Little Snitch interaction model (layout and
behaviour only; no LS logo, icons, artwork or copy). Later phases (monitor tree +
world map, rules window + factory groups, sidebar shell) are separate PRs.

## Evidence that drives Phase 1

- Host 2026-10-12: `/var/log/opensnitchd.log` shows 15x "Added new rule" and 0x
  "Saved new rule" after the owner clicked Allow. The daemon writes a rule to disk
  only for `duration == always` (`vendor/opensnitch/daemon/rule/loader.go`).
- The bridge advertises `appBoundRules` unconditionally
  (`crates/snitchwatch-bridge/src/bridge_capabilities.rs`), so the bridge is not
  the blocker.
- Inline Allow always sends `this_time`: `InlineVerdicts.qml` asks
  `ConnectionsModel.inlineDurationFor`, which returns `this_time` for every Allow
  (`src/inline_deny.rs` `duration_token_for_row`). Notifications do the same
  (`src/notification_actions.rs`). A second downgrade path exists:
  `limit_to_bridge` sends `Once` whenever the caller says the process path is not
  bindable.
- Closing the window only hides it (`main.qml` `onClosing`); the WebSocket session
  stays authenticated, so every unanswered ask waits the full 30 s, one at a time.

## Owner decisions (2026-10-12)

1. Durations offered: **Once / 5 minutes / Until firewall restarts / Forever** — each maps
   to a distinct daemon duration. A real "Until Quit" (bridge tracks process exit)
   is deferred. The control is built from one list so this is a one-line change.
2. Flatpak gets **read-only** access to host `.desktop` files and icon themes for
   app icons.
3. Silent mode offers Allow+record (preselected) and Deny+record. Deny+record shows
   a warning and offers the existing curated-defaults opt-in alongside it.

## Phase 0 — make the look reviewable

- `qml/Theme.qml` singleton: allow green, deny red, partial gray, needs-rule blue,
  pending orange; spacing and type scale.
- ConnectionsModel roles: `processPath`, `direction`, `startedAtMs`, `appIcon`.
- App icon lookup in Rust: process path -> `.desktop` (`Exec`/`TryExec`) -> `Icon`,
  generic fallback. Flatpak manifests gain read-only grants.
- Demo mode (`SNITCHWATCH_DEMO=1`): fake rows, no bridge. Screenshot script:
  offscreen `grabToImage` of each key surface to a directory.

## Phase 1 — alert and behaviour

- Standalone alert window: identity first (icon, program, path), then destination.
  Scope: any / domain / host. Duration: segmented control, preselected from a new
  Settings value. Return = allow, Esc = deny, keys ignored for 1 s after it appears.
- Inline and notification "Allow" use the Settings duration, not `this_time`.
- Silent mode, chosen in onboarding, stored in Settings. When silent mode is
  Allow or Deny, Rust answers each new ask at arrival (and each ask already
  waiting when the mode is switched) with `Once` + the chosen verdict and marks
  the row "needs a rule". Only one session answers. **Owner decision (replaces
  "a hidden window behaves like silent mode"):** the alert is its own window,
  independent of the main window. In "Ask me" it shows for every new prompt
  whether the main window is visible or hidden to the tray, nothing is
  auto-answered, a new prompt never raises the main window, and no duplicate
  desktop notification is sent. Only silent Allow/Deny answer silently.
- After the GUI quits, the bridge reports no GUI session and the daemon applies its
  own DefaultAction. Deny+record cannot apply then; the UI says so.

## Tests (write first)

- Bridge round trip (`MockOpensnitchd`, `ask_rule_round_trip_*` pattern): a
  Forever verdict for a bindable program yields an AskRule reply with
  `duration == "always"`.
- Kirigami unit: `limit_to_bridge` with app-bound rules and a bindable path does
  not downgrade; inline Allow token follows the Settings duration.
- Text guards (`qml_source_guards.rs`, `honest_ui_*`) updated to the new intent,
  never deleted.

## Verification

`just check`, `just test`; `cargo build/clippy -D warnings/test -p
snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`;
`just qml-test`. Screenshots checked non-blank; captured with Basic style (the
Flatpak uses the platform style, so colours/metrics differ slightly).
