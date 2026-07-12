# Tray state: DaemonDown + RecentBlock wiring

**Date:** 2026-07-12
**Status: DONE.** Both variants landed as designed below. `cargo test -p
snitchwatch-bridge` (192 passed, including the 5 new `daemon_watchdog`
tests and 2 new `grpc_server` tests), `cargo test -p snitchwatch-bridge-cli`,
the root `bridge_protocol` integration test, `cargo test -p
snitchwatch-kirigami` (246 passed, unaffected), and workspace-wide `just
check`/`cargo test --workspace` all pass. `FilterOff` remains unbuilt per
its own non-goal below.

**Follow-up to:** `.agent_native/agent_roadmap.md` item 9 (tray state was
display-only in production; `Idle`/`Pending(n)` fixed separately in
`b902a2a`). This covers the two remaining variants scoped as "buildable
today" — `FilterOff` stays out (no backing feature exists, needs a product
decision).

## Design

### DaemonDown

opensnitchd's own poller (`vendor/opensnitch/daemon/ui/client.go`'s
`poller()` loop) connects-or-pings once per second while connected
(`time.Sleep(1 * time.Second)` between iterations) — this is a read-only
reference fact from the vendored submodule, not something this repo
controls. `grpc_server.rs`'s `ping()` handler is therefore already the
daemon's heartbeat; the bridge just never watched it for staleness.

- `UiService` gains `last_ping: Arc<std::sync::Mutex<Instant>>`
  (constructed internally in `new()`, not a new constructor parameter — no
  existing call site needs to change), updated to `Instant::now()` at the
  top of `ping()`, unconditionally (every ping, not just ones carrying
  stats).
- A new pure function, `daemon_watchdog::is_daemon_down(last_ping, now,
  timeout) -> bool`, trivially unit-tested without async runtime.
- `DAEMON_DOWN_TIMEOUT = Duration::from_secs(10)`: 10x the observed ~1s
  ping cadence — generous enough to absorb a dropped ping or scheduling
  jitter without false-positiving, tight enough to notice a real outage
  quickly. Documented as an assumption, not a measured SLA.
- `daemon_watchdog::run` is an async loop (ticks every 2s) spawned from
  `snitchwatch-bridge-cli::run` (which already holds `tray_pub`/`cache`
  handles) via `UiService::last_ping_handle()` (a new accessor, not a
  constructor change). Tracks a local `was_down: bool`; on the down→up
  edge, resyncs the tray to the cache's actual current state (see below)
  rather than assuming `Idle`.

### RecentBlock

- `UiService` gains `block_generation: Arc<AtomicU64>`, initialized like
  the existing `next_ask_id` counter.
- In `ask_rule`, after `cache.resolve(...)` succeeds with `Verdict::Deny`:
  increment the generation counter, publish `TrayState::RecentBlock { what,
  ttl }` (`what` = `"<process> → <dst_host>"`, mirroring the tray tests'
  existing format; `ttl = Duration::from_secs(5)`, a UX default with no
  prior precedent in this codebase to match — documented as such, easy to
  tune later), then spawn a revert timer capturing its own generation
  number. When the timer fires, it only reverts if the generation is still
  current (no newer block superseded it) — otherwise it's a no-op, since
  the newer block's own timer owns the eventual revert. This avoids two
  concurrent blocks' timers racing to stomp each other's state.

### Shared: resync-from-cache

Both recovery paths (`DaemonDown` clearing, a `RecentBlock` timer firing)
need "what should the tray show right now, given actual cache state" — not
a hardcoded `Idle`. `ConnectionCache` gains one new public method,
`resync_tray_state(&self)`, a thin wrapper around the existing private
`republish_pending_count` (unchanged, still used internally by
`insert_pending`/`resolve` too) so there's exactly one place that maps
"current pending count" to `Idle`/`Pending(n)`.

## Non-goals

- `FilterOff`: out of scope, no backing feature exists (see roadmap item 9).
- Changing `ping()`'s existing stats-handling behavior — only adding the
  timestamp update, nothing else in that handler changes.
- Any change to `vendor/opensnitch` (read-only submodule; only read from it
  to confirm the ping cadence assumption above).

## Testing

- `is_daemon_down`: pure unit tests, no runtime.
- Watchdog down→up→down transitions: `#[tokio::test(start_paused = true)]`
  with `tokio::time::advance`, matching the existing pattern in
  `crates/snitchwatch-kirigami/src/insight/client.rs`.
- `RecentBlock` generation-guard: a test that fires two blocks within each
  other's `ttl` and asserts only the second one's timer actually reverts
  the tray (the first's is a no-op).
- Existing `ping()`/`ask_rule` tests in `grpc_server.rs` must keep passing
  unchanged — this is additive, not a behavior change to their existing
  contracts.
