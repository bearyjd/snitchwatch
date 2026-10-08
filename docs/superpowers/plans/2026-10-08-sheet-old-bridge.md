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
  > so with this scope it can only answer this connection. Update
  > Snitchwatch's background service to remember answers."
  - The inline Deny's sentence ("...too old to block just this program...")
    is worded for its own action. A test only requires both to say "too
    old" and "this program".
- **"Any host" is unaffected on an old bridge, for an identifiable
  program.** That rule matches the program alone, by design. An
  unidentifiable program (#44) is never remembered under any scope: on an
  old bridge an empty process path becomes a host-only rule for every app.
- **One per-session check.** The sheet reuses the inline Deny's check,
  `InlineVerdicts.rowAppBoundRules` → `BridgeFeed.appBoundRulesFor` →
  `app_bound_rules_for_row`.
  - #74 already sets `sheet.appBoundRules` from it when the inspector opens.
- **No path can submit a persistent duration on such a bridge.**
  - **QML.** `submit()` derives the duration from the gate and the scope it
    is about to send. It never trusts the selector's state: keyboard input,
    a stale index or a pre-selected duration all fall back to "This time".
  - **Rust.** `BridgeFeed::submitVerdict` takes a fifth argument,
    `bindable_process_path`: `rowDetailsJson`'s `bindableProcessPath` for the
    row, which the bridge crate's `is_bindable_process_path` computes. The
    sheet sends its raw `bindableProcessPath`, never its own `remembers`
    result, and the inline buttons send `rowBindableProcessPath(row)`.
    `bridge_feed::dispatch_to` then downgrades a `SetVerdict` that asks to be
    remembered (any duration but `once`, the legacy `remember: true`
    included) to `once` unless **both** hold:
    - the caller reports a bindable program, whatever the scope;
    - the session advertised app-bound rules, or the scope is `any_host`.

    The rule is `pending_decision::limit_to_bridge`. It also returns whether
    it changed anything, and `dispatch_to` logs a `warn!` with the row id
    and scope when it did: the sheet offers no such choice, so that means a
    QML check was bypassed.
  - **What Rust does not guarantee.** `BridgeFeed` cannot look the row up:
    the row store belongs to `ConnectionsModel`. Rust therefore trusts the
    caller's `bindable_process_path`. It protects against a sheet or inline
    path that mis-gates or mis-selects a duration, not against a caller that
    reports a wrong flag. `sendClientJson` passes `false`: a JSON verdict
    vouches for no program, so a remembered one goes out once-only (no
    production caller sends verdicts that way).

## Tests first

- **Rust, pure.** A table over scope × duration × capability × bindable
  program, including the legacy `remember`, that checks the `changed` flag
  too: true for every downgrade, false for every kept message.
- **Rust, through `dispatch_to`.** Use a new `bridge_runtime` test module,
  because `tests.rs` is close to 800 lines. Check, for Allow and Deny:
  - a session without the capability → `this_host`/`any_host_on_domain`
    remembered verdicts are queued as `once`;
  - `any_host` is kept for a bindable program;
  - a capable session is kept;
  - an unbindable program → `once` under every scope, on an old and a
    capable session;
  - a downgrade logs a warning naming the row and scope; a kept verdict logs
    nothing.
- **QML probe** (new `sheet_old_bridge_qml.rs`, strict stderr filter):
  - old bridge: `this_host` and `any_host_on_domain` offer only "This
    time", and the note shows;
  - "Any host" offers all four durations;
  - Forever chosen under "Any host", then the scope switched back, submits
    `this_time`;
  - a forced selector or index submits `this_time`;
  - the `kernel` row and a `processPath: null` row, on an old bridge, under
    every scope, with Forever pre-selected or forced: one duration, `this_time`
    sent, only the #44 note;
  - every send carries the row's real `bindableProcessPath`;
  - a capable bridge keeps "Forever";
  - the two "too old" sentences agree on the cause and "this program".
- `inline_verdict_qml.rs` checks the flag the inline buttons send, so a
  constant `true` (which would send a remembered verdict for an
  unidentifiable program) or `false` (which would undo the inline Deny's
  `until_quit`) fails.
- `verdict_not_remembered_qml.rs`'s stub becomes a capable bridge.
- **Mutations:**
  - remove the QML gate; weaken it to
    `(bindableProcessPath && appBoundRules) || scope === "any_host"`;
  - remove the Rust program check; ignore the caller's flag in
    `dispatch_to`;
  - make `limit_to_bridge` always report no change; drop the `warn!`;
  - send a constant from the sheet or the inline buttons;
  - let a persistent duration through `submit()`.

Also fix the stale `ConnectionsPage.rowAppBoundRules` comment in
`PendingDecisionSheet.qml`.

## Out of scope

A capability-changed notify for the inline tooltips (optional, see the
report). The capability is fixed per session, and the 1 s `ok` poll covers
disconnects.

## Known gaps

- A sheet opened right after its session died can read `appBoundRules ==
  false` and show the "too old" note for at most the 1 s until the `ok` poll
  closes it. Cosmetic: the sheet then offers only "This time" (the safe
  side) and the verdict is rejected as stale anyway.
