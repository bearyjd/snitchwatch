# Stuck pending rows after the daemon's ask timeout (issue #49, third bullet)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #49 (roadmap P0.6), bullet "Stuck pending rows"
**Baseline:** `main` @ `670f42c`.

The other two bullets are covered by the honest-ui PR (branch
`fix/honest-ui`):
- the never-set countdown: its diff removes the countdown from
  `PendingDecisionSheet.qml`;
- the zeroed per-connection bytes and sparkline: it stops passing
  `trafficModel` into `ConnectionsPage` from `main.qml`, and
  `honest_ui_qml_guards.rs` `per_connection_traffic_readouts_stay_hidden`
  guards that.

**Verdict:** the bridge side is fixed by #39 and proven by its tests. What
remains is one end-to-end regression test, a GUI fix so a removed pending
row can't still be answered, and a manual check against a real daemon.

**Size:** S.

## Goal

1. Prove the bridge-side fix end to end, through a real bridge, a WS
   client, and a deadline-carrying `AskRule`.
2. The GUI never offers a verdict for a pending row that no longer exists,
   whether it was removed, cleared, or lost to a session change.

## Out of scope

- The countdown and byte-count bullets (honest-ui PR).
- Recording a timed-out request in history as "timed out → default action"
  instead of removing the row (see Risks).

## Citation convention

- `main:` means `670f42c`. Functions and tests are cited by name. Line
  numbers are approximate.
- `vendor:` means opensnitch v1.8.0.

## Why rows got stuck

- opensnitchd gives each `AskRule` a 120 s context deadline
  (`vendor:daemon/ui/client.go` `Client.Ask`,
  `context.WithTimeout(…, time.Second*120)`), then applies its default
  action.
- Before #39, the bridge's `ask_rule` waited on the verdict oneshot with no
  cleanup when the RPC went away, so the row stayed pending.
- The GUI works around this (HANDOFF.md, 2026-08-05): the pending-exposure
  banner ignores rows older than 120 s.

## Evidence that #39 fixes the bridge side (`main`)

- **Cleanup on drop.** `grpc_server.rs` `ask_rule` creates a
  `PendingCleanup` right after `insert_admitted`. Its `Drop` runs when
  tonic drops the handler future, which happens on client cancellation:
  grpc-go's deadline sends `grpc-timeout` and then `RST_STREAM`. The drop
  calls `ConnectionCache::cancel_pending`, which:
  - removes the row;
  - broadcasts `RemoveConnectionRows`;
  - republishes the tray.

  Under cache-lock contention, the drop spawns that work as a task.
- **Late verdicts are rejected.** The verdict receiver is declared after
  the cleanup guard, so it is dropped first. `ConnectionCache::resolve` on
  a dropped receiver removes the row and returns `Err`.
- **No other production path creates pending rows.** Every one goes
  through `insert_admitted`. The other `insert_pending` callers sit in
  `#[cfg(test)]` modules (`daemon_watchdog.rs` tests,
  `translator/upstream.rs` tests).
- **Tests** (`grpc_server/tests.rs`):
  - `tonic_request_deadline_cleans_pending_with_silent_authenticated_gui`
    is the exact #49 scenario. An authenticated silent GUI and a tonic
    client with `set_timeout(100 ms)` produce `RemoveConnectionRows`, an
    empty cache, and a server that still answers `ping`.
  - `tonic_client_abort_removes_real_server_pending_request`
  - `rpc_future_drop_under_cache_contention_rejects_verdict_and_cleans_row`
- **VM evidence.** `docs/packaging/system-bridge-integration.md`
  ("Unattended requests") records RPC cancellation removing the row, and
  synthetic protected-IPC cancellation checks passing with enforcing
  SELinux.

## Remaining gaps (the work)

1. **No end-to-end test of the timeout path** through
   `snitchwatch_bridge_cli::run` and a WS client.
   `MockOpensnitchd::ask_rule` uses a local `tokio::time::timeout`, not a
   gRPC deadline.
2. **The GUI keeps a vanished pending row actionable.**
   - `ConnectionsPage.qml` `openInspector` copies the row's fields into
     page properties, including `inspectPending`. The embedded
     `PendingDecisionSheet` is shown from that copy.
   - Nothing re-checks it when:
     - the row is removed (`ConnectionsModel::apply_remove`);
     - the model is cleared (`ClearConnectionRows`, which the snapshot
       answer sends first);
     - the GUI reconnects. Row ids are qualified with the session's
       connection id (`qualify_connection_ids`), so after a reconnect the
       held id never exists again.
   - A late Allow click is then rejected by the bridge (`resolve` returns
     `NotPending`, and the pump logs "upstream apply failed"), but the sheet
     closes as if it succeeded.
3. **No real-daemon 120 s check** with a silent GUI is recorded.

## Tests to write first

1. **Mock scaffolding.** Add
   `MockOpensnitchd::ask_rule_with_deadline(conn, Duration)` using
   `Request::set_timeout`, as grpc-go does.
2. **End-to-end test** (`tests/bridge_protocol_test.rs`, following
   `last_real_gui_disconnect_removes_prompt_and_rejects_late_persistent_verdict`):
   1. connect an authenticated WS client that never answers;
   2. send an `AskRule` with a 300 ms deadline;
   3. expect `InsertConnectionRows`, then `RemoveConnectionRows` for the
      same id, then `TrayState` back to `Idle`;
   4. a late `SetVerdict` for that id produces no `UpdateConnectionRows`
      and no `UpdateRules`.

   This pins existing behavior. Show it can fail by removing the
   `PendingCleanup` construction in a scratch run.
3. **QML tests** (existing `crates/snitchwatch-kirigami/tests/*_qml.rs`
   style). Each opens the inspector on a pending row and then triggers one
   of these:
   - `RemoveConnectionRows` for that row;
   - `ClearConnectionRows`;
   - a simulated session change (new connection id, then a snapshot).

   In each case, the decision sheet is hidden and the "no longer pending"
   message is shown. These fail until design step 1 lands.

## Design

1. **GUI fix.**
   - Add a `ConnectionsModel` invokable, `isPendingRow(id) -> bool`.
   - In `ConnectionsPage.qml`, add one `recheckInspectedRow()` that runs:
     - on the model's standard `rowsRemoved` and `modelReset` signals
       (`apply_remove`; `ClearConnectionRows` and the debounced snapshot
       path both use `begin_reset_model`/`end_reset_model`);
     - when the bridge feed's session or connection status changes.
   - When `page.inspectPending` is set and the row is no longer pending, it
     sets `inspectPending = false` and shows "This request is no longer
     pending: it timed out or the connection to the background service was
     lost; opensnitchd applied its default action".
   - Don't auto-close the inspector.
   - Land this after the honest-ui PR, which modifies `ConnectionsPage.qml`
     and `PendingDecisionSheet.qml`.
2. **Keep the GUI's 120 s exclusion** in the pending-exposure banner
   (`ConnectionsModel` `oldestPendingAgeSecs`). The published v0.1.1
   tarball predates #39. Update its comment to say the bridge reaps rows as
   of #39.
3. **Manual VM check.** With the system bridge and a GUI attached:
   1. don't answer a real prompt;
   2. after about 120 s, the row disappears and the tray goes back to Idle;
   3. `journalctl -u opensnitchd` shows "Error while asking for rule".

   Record the result in HANDOFF.md and close the bullet.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-bridge grpc_server`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
  and `QT_QUICK_CONTROLS_STYLE=Basic`

## Risks and open questions

- **A timed-out request disappears from history.** Turning it into a
  decided "timed out → default action" row would be a more honest record.
  That is a product follow-up.
- **"Session changed" depends on the feed.** The QML must reach the bridge
  feed's session or connection-status signal. **Verify** which signal
  `BridgeFeed` exposes after #38/#39 before writing the test.
