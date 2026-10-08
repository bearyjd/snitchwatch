# Stuck pending rows after the daemon's ask timeout (issue #49, third bullet)

**Date:** 2026-10-07
**Issue:** #49 (roadmap P0.6), bullet "Stuck pending rows"

The other two bullets are covered by the honest-ui PR (branch
`fix/honest-ui`, uncommitted at time of writing):
- the never-set countdown: its diff removes the countdown block from
  `PendingDecisionSheet.qml`;
- the zeroed per-connection bytes and sparkline: its diff drops
  `trafficModel` from `ConnectionsPage` in `main.qml`, and
  `honest_ui_qml_guards.rs` `per_connection_traffic_readouts_stay_hidden`
  guards it.

**Verdict:** #39 fixes the bridge side, and its tests prove it. What
remains is one end-to-end regression test, one small GUI fix, and a manual
check against a real daemon.

**Size:** S.

## Goal

1. The bridge-side fix in #39 is proven end to end, through a real
   bridge, a WS client, and a deadline-carrying `AskRule`, not only at the
   `UiService` unit level.
2. The GUI never offers a verdict for a pending row that has been removed.

## Out of scope

- The countdown and byte-count bullets of #49. The honest-ui PR covers
  them; see above.
- Recording a timed-out request in history as "timed out → default
  action" instead of removing the row (see Risks).

## Citation convention

- `#39:` is PR #39 at `5c2b44a`.
- `main:` is `f65a2a4`.
- `vendor:` is opensnitch v1.8.0.

## Why rows got stuck

opensnitchd gives each `AskRule` a 120 s context deadline
(`vendor:daemon/ui/client.go:360-379`, `context.WithTimeout(…, time.Second*120)`
at `:366`). It then applies its default action.

The original fault: on `main`, the bridge's `ask_rule` waited on the
verdict oneshot with no cleanup when the RPC went away, so the pending row
stayed in the cache. The GUI worked around it (HANDOFF.md, 2026-08-05):
the pending-exposure banner ignores rows older than 120 s, because "the
bridge has no reaper for those".

## Evidence that #39 fixes the bridge side

- **Cleanup on drop.** `PendingCleanup`
  (`#39:crates/snitchwatch-bridge/src/grpc_server.rs:257-277`) is created
  right after the pending row is admitted (`ask_rule`, `:535-538`). Its
  `Drop` calls `ConnectionCache::cancel_pending`, which:
  - removes the row;
  - broadcasts `RemoveConnectionRows`;
  - republishes the tray count (`#39:cache/connections.rs:220-239`).

  Tonic drops the handler future when the client cancels: grpc-go's
  context deadline sends `grpc-timeout` and then `RST_STREAM`. The cleanup
  also runs under cache-lock contention, by spawning a task.
- **Late verdicts cannot revive a row.** The receiver is dropped first
  (`ask_rule`, `:539-540`, "Declared after cleanup so cancellation drops
  the receiver first"). `resolve` on a dropped receiver removes the row and
  returns `Err` (`connections.rs:185-194`).
- **No other production path creates pending rows.** Every one goes
  through `insert_admitted` (`ask_rule`, `:517-523`). The two other
  `insert_pending` callers are inside `#[cfg(test)]` modules:
  `daemon_watchdog.rs:145` and `translator/upstream.rs:382`.
- **Tests on #39** (`#39:crates/snitchwatch-bridge/src/grpc_server/tests.rs`):
  - `tonic_request_deadline_cleans_pending_with_silent_authenticated_gui`
    (`:1227-1271`) is the exact #49 scenario. An authenticated but silent
    GUI and a real tonic client with `set_timeout(100 ms)` produce
    `RemoveConnectionRows`, an empty cache, and a server that still serves
    `ping` afterwards;
  - `tonic_client_abort_removes_real_server_pending_request` (`:1158-1192`);
  - `rpc_future_drop_under_cache_contention_rejects_verdict_and_cleans_row`
    (`:1132-1155`).
- **VM evidence.** #39's `docs/packaging/system-bridge-integration.md:36-41`
  records that RPC cancellation removes the pending row, and that "an
  authenticated but silent GUI still uses the daemon's existing RPC
  deadline". It also records synthetic protected-IPC RPC-cancellation
  checks passing on a VM with enforcing SELinux.

## Remaining gaps (the work)

1. **No end-to-end test of the timeout path.**
   - The unit test above drives `UiService` directly. Nothing checks that
     a real bridge (`snitchwatch_bridge_cli::run`) plus a WS client sees the
     removal.
   - `MockOpensnitchd::ask_rule` uses a local `tokio::time::timeout` and no
     gRPC deadline (`tests/mock_opensnitchd/src/lib.rs` `ask_rule`), so it
     cannot model the daemon's real deadline.
2. **The GUI keeps a removed pending row actionable.**
   - `ConnectionsPage.openInspector` copies the row's fields into page
     properties (`main:crates/snitchwatch-kirigami/qml/ConnectionsPage.qml:470-482`,
     `inspectPending` at `:477`). The embedded `PendingDecisionSheet` is
     visible on that copy (`:566-578`). Nothing closes the inspector when
     the row is removed: `inspector.close()` runs only on "Show rule" and
     `onDecided`.
   - After a timeout, the user can still click Allow. The bridge rejects the
     verdict: `resolve` returns `NotPending` and the pump logs "upstream
     apply failed" (`#39:crates/snitchwatch-bridge-cli/src/lib.rs:639`). The
     sheet then closes as if the click succeeded.
3. **No real-daemon check at 120 s with a silent GUI** is recorded for
   the row-removal outcome.

## Tests to write first

1. **Mock scaffolding.** Add
   `MockOpensnitchd::ask_rule_with_deadline(conn, Duration)`. It calls
   `Request::set_timeout`, so tonic sends `grpc-timeout` and cancels at the
   deadline, the way grpc-go does.
2. **End-to-end test.** Add to `tests/bridge_protocol_test.rs`, following
   `last_real_gui_disconnect_removes_prompt_and_rejects_late_persistent_verdict`
   (#39 `:706`):
   1. boot a bridge;
   2. connect an authenticated WS client that never answers;
   3. send an `AskRule` with a 300 ms deadline;
   4. assert that the client receives `InsertConnectionRows`, then
      `RemoveConnectionRows` for the same id, then `TrayState` back to
      `Idle`;
   5. assert that a late `SetVerdict` for that id produces no
      `UpdateConnectionRows` and no `UpdateRules`.

   On #39 this test should pass immediately, because it pins existing
   behavior. Prove it can fail with a sabotage run: comment out the
   `PendingCleanup` construction in `ask_rule` and check the test fails.
3. **QML test** (existing `crates/snitchwatch-kirigami/tests/*_qml.rs`
   style):
   1. open the inspector on a pending row;
   2. inject `RemoveConnectionRows` for it;
   3. assert the decision sheet is hidden and the timed-out message is
      shown.

   This test fails until design step 1 lands.

## Design

1. **GUI fix.** In `ConnectionsModel`, emit `rowRemoved(id)` from
   `apply_remove` (`connections_model.rs:998`). In `ConnectionsPage.qml`,
   when `id === page.inspectId && page.inspectPending`:
   - set `inspectPending = false`;
   - show an inline "This request timed out; opensnitchd applied its
     default action" message instead of the decision sheet.

   Do not auto-close the inspector; that would yank the panel away
   mid-read. `PendingDecisionSheet.qml` and `ConnectionsPage.qml` are both
   modified by the honest-ui PR, so land this after it.
2. **Keep the GUI's 120 s exclusion** in the pending-exposure banner
   (`ConnectionsModel.oldestPendingAgeSecs`). The published v0.1.1 bridge
   tarball predates #39 and still leaves stuck rows. Update its comment to
   say the bridge reaps rows as of #39.
3. **Manual VM check.** With the system bridge and a GUI attached:
   1. trigger a real connection and don't answer;
   2. after about 120 s, the row disappears and the tray returns to Idle;
   3. `journalctl -u opensnitchd` shows "Error while asking for rule" and
      the default action applied.

   Record the result in HANDOFF.md and close #49's third bullet with it.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo test -p snitchwatch-bridge grpc_server`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
  and `QT_QUICK_CONTROLS_STYLE=Basic`

## Risks and open questions

- **A timed-out request disappears from history.** The row is removed, not
  turned into a decided row such as "timed out → default action". That is
  arguably the more honest record. It is a product follow-up, not a
  regression.
