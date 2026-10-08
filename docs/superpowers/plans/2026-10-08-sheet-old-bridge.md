# Decision sheet on bridges without app-bound rules (#72)

**Date:** 2026-10-08
**Issue:** #72 (refs #44). Follow-up to #74, the inline Deny until restart.
**Baseline:** `main` @ `b224adf`.
**Size:** S. Kirigami only. No protocol change.

## Problem

Some bridges don't advertise the `appBoundRules` capability (`bridge_capabilities.rs`):
- every bridge before #50/#71, including v0.1.1 `9fdb336`;
- the #39 system-bridge head `5c2b44a`.

They build "This host only" and "Any host on this domain" rules without a
`process.path` condition, so the rule covers **every app**. The sheet still
offers "Until firewall restarts" and "Forever" for those scopes. On such a
bridge, Allow + This host + Forever lets every app reach that host forever.
That is the original #44 bug.

## Decision (orchestrator, logged for the owner): fail-safe

- **Persistent durations are blocked.** When the row's bridge session did
  not advertise `appBoundRules`, the sheet offers only "This time" for
  `this_host` and `any_host_on_domain`. It says why, in plain text:
  > "This firewall bridge is too old to limit a rule to just this program,
  > so it can only answer this connection. Update Snitchwatch's background
  > service to remember answers."
- **"Any host" is unaffected.** That rule matches the program alone, by
  design.
- **One per-session check.** The sheet reuses the inline Deny's check,
  `InlineVerdicts.rowAppBoundRules` → `BridgeFeed.appBoundRulesFor` →
  `app_bound_rules_for_row`.
  - #74 already sets `sheet.appBoundRules` from it when the inspector opens.
- **No path can submit a persistent duration on such a bridge.**
  - **QML.** `submit()` derives the duration from the gate and the scope it
    is about to send. It never trusts the selector's state: keyboard input,
    a stale index or a pre-selected duration all fall back to "This time".
  - **Rust.** `bridge_feed::dispatch_to` downgrades any `SetVerdict` it
    sends with a remembered duration and a `this_host` or
    `any_host_on_domain` scope to `once`, unless
    `app_bound_rules_for_row` says the session can bind it. This covers
    every QML caller, including the legacy `remember: true`. The pure rule
    lives in `pending_decision.rs`.

## Tests first

- **Rust, pure.** A table over scope × duration × capability, including the
  legacy `remember`.
- **Rust, through `dispatch_to`.** Use a new `bridge_runtime` test module,
  because `tests.rs` is close to 800 lines. Check:
  - a session without the capability → `this_host`/`any_host_on_domain`
    remembered verdicts are queued as `once`;
  - `any_host` is kept;
  - a capable session is kept.
- **QML probe** (new `sheet_old_bridge_qml.rs`, strict stderr filter):
  - old bridge: `this_host` and `any_host_on_domain` offer only "This
    time", and the note shows;
  - "Any host" offers all four durations;
  - Forever chosen under "Any host", then the scope switched back, submits
    `this_time`;
  - a forced selector or index submits `this_time`;
  - a capable bridge keeps "Forever".
- `verdict_not_remembered_qml.rs`'s stub becomes a capable bridge.
- **Mutations:**
  - remove the QML gate;
  - remove the Rust downgrade;
  - let a persistent duration through `submit()`.

Also fix the stale `ConnectionsPage.rowAppBoundRules` comment in
`PendingDecisionSheet.qml`.

## Out of scope

A capability-changed notify for the inline tooltips (optional, see the
report). The capability is fixed per session, and the 1 s `ok` poll covers
disconnects.
