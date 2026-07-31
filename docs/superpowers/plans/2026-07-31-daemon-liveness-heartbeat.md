# Daemon Liveness Heartbeat Fix (issue #5)

**Goal:** Stop `daemon_watchdog`/diagnostics from false-positiving
`DaemonDown` on a healthy idle daemon. Verified live (2026-07-31): real
opensnitchd v1.8.0 only sends the `Ping` RPC when it has *new stats
events* (`vendor/opensnitch/daemon/ui/client.go:329`,
`daemon/statistics/stats.go:266`) — an idle daemon stays connected but
silent, and the bridge's ping-staleness heartbeat declares it down.

**Approved design (issue #5 option 1):** liveness = *any* inbound
daemon-originated gRPC activity, with the long-lived Notifications
stream's open/closed state as the authoritative signal:

- `DaemonLiveness` (new, in `grpc_server.rs`): wraps
  `last_activity: Arc<StdMutex<Instant>>` + `open_notification_streams:
  Arc<AtomicUsize>`.
- Every daemon-facing handler (`ping`, `ask_rule`, `subscribe`,
  `post_alert`, `notifications` open + each reply message) refreshes
  `last_activity`. `notifications` increments the stream counter on open
  and decrements when the reply loop ends (daemon side closed).
- Down predicate: daemon is down iff **no notification stream is open
  AND `last_activity` is stale** (existing `DAEMON_DOWN_TIMEOUT`). An
  open stream means alive regardless of staleness — that is exactly the
  idle-daemon shape observed live. The staleness fallback covers a daemon
  that never opens the stream.
- `daemon_watchdog::run` and `DiagnosticsCtx` consume `DaemonLiveness`
  instead of the bare `last_ping` handle. `last_ping_handle()` stays (it
  feeds `DiagnosticsCtx` and tests) but is renamed conceptually via
  `liveness_handle()`; keep a thin alias only if call-site churn is big.

**Mock gap being closed:** `MockOpensnitchd` pings unconditionally, which
is why sandbox tests never caught this. Add mock support for (a) opening
a Notifications stream and holding it, (b) connecting *without* pinging.
The new load-bearing test: daemon connected, stream open, zero pings for
> `DAEMON_DOWN_TIMEOUT` → tray stays `Idle`, diagnostics report
`daemon_reachable: Ok`. Second test: stream closes → down transition
fires (tray `DaemonDown` + `DiagnosticsReport` broadcast) without waiting
for ping staleness beyond one watchdog tick.

**Non-goals:** surfacing daemon-side eBPF errors via Notifications
(issue #6, separate); changing `firewall_running` capture (its staleness
is mitigated by this fix's accurate reachability; full invalidation can
ride issue #6's Notifications work).

## Tasks

1. `DaemonLiveness` type + down predicate in `grpc_server.rs`
   (unit-TDD: stale-but-stream-open = alive; stale-and-no-stream = down;
   fresh-activity = alive).
2. Refresh `last_activity` in all daemon-facing handlers; wire stream
   open/close counting in `notifications` (unit tests via existing
   `spawn_test_service` pattern).
3. `daemon_watchdog::run` takes `DaemonLiveness`; update
   `DiagnosticsCtx::new` to take it too; adjust `snitchwatch-bridge-cli`
   wiring and all existing tests' constructor calls.
4. `MockOpensnitchd`: `open_notifications()` + no-ping connect mode; new
   integration tests in `tests/bridge_protocol_test.rs` for the two
   scenarios above.
5. Runbook: replace the issue #5 caveat in Step 3 with "fixed by this
   plan" note; `just check`, `just test`, kirigami tests offscreen.

Verification: full workspace suites + re-run live Step 6b on real
hardware (manual, post-merge).
