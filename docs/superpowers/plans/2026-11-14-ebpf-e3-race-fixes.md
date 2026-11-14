# E3 review round 4: fallback-state races (PR #353)

Codex adversarial review of #353 at head 2346b201 found 0 CRITICAL/HIGH and 3 MEDIUM
findings, all about the eBPF-to-/proc fallback flag (`MonitorFallback` generic alert)
having no ordering or session identity.

## Fixes (TDD: write the failing test first for each)

1. **Watchdog clears a replacement daemon's fresh fallback**
   (`daemon_watchdog.rs:62`). The watchdog samples `down_now`, awaits the cache lock,
   then clears unconditionally. Fix: sample `Instant::now()` with `down_now` and add
   `AlertStore::clear_monitor_fallback_recorded_before(sampled_at)` (and a
   `DiagnosticsCtx` wrapper). An alert recorded after the sample survives.
   Test: record a fallback with `recorded_at` after the sample, run the transition,
   assert it is kept and `process_monitor` is still `Some`.

2. **Stale fallback snapshot republished after a concurrent clear**
   (`grpc_server.rs:621`, also the Subscribe push at ~569 and the watchdog sends at
   65/78). The state read (`ctx.report()` + `ctx.process_monitor()`) and the
   `broadcast.send` are not atomic with respect to other publishers. Fix: one
   `DiagnosticsCtx::publish(&broadcast)` helper that holds a publish lock across
   snapshot + send; every site that builds a `DiagnosticsReport` uses it, and each
   state mutation happens before its publish call.
   Test: interleave a PostAlert snapshot with a Subscribe clear (deterministic via the
   lock), assert the last broadcast is the post-clear state.

3. **Alert age is not daemon-session identity** (`daemon_alerts.rs:226`).
   NOT fixable in this repo: opensnitchd's `ClientConfig.id` is `ts.UnixNano()` per
   Subscribe (vendor `daemon/ui/notifications.go:52`), not per process, and nothing else
   in Subscribe identifies a process. Transport reconnect and replacement daemon look
   the same. Keep the 3 s grace, document the limitation in the `on_daemon_subscribe`
   doc comment and the E3 plan, and file an issue for the upstream-field option
   (STATE.md E3 option 3). Accepted deviation, to be stated in the PR description.

## Gate

`just check`, `just test-bridge`, `just test` green; fresh `/codex` challenge on the new
head; merge on green CI (`--match-head-commit`); send the tower the merged SHA.
