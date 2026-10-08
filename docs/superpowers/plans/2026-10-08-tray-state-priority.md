# Tray state priority (#58)

**Date:** 2026-10-08. **Baseline:** `main` @ `b224adf`. **Size:** S, bridge only.

## Problem

The watchdog sets `DaemonDown` only on the daemon-down transition. Every
other tray resync recomputes the state from pending rows and the pause flag
alone, so it flips the tray back to Idle/Pending while the daemon is still
down. Those resyncs are `ConnectionCache::republish_pending_count`, which
runs when:
- a prompt is inserted, resolved or cancelled;
- a recent block reverts;
- a pause starts, ends or expires (since #47).

## Fix

- **Derive the tray state in one pure function.**
  `TrayState::derive(daemon_down, paused, pending)` in `tray_state.rs` has
  the issue's priority: **DaemonDown > FilterOff (paused) > Pending(n) >
  Idle**.
- **The cache keeps the daemon state as an input.**
  - `ConnectionCache` gets a `daemon_down` flag, set and cleared by the
    watchdog.
  - Every republish publishes `tray_state()`, which is that function over
    the cache's inputs.
- **The watchdog** sets the flag and publishes the derived state on both
  transitions. On recovery that is still whatever the pending rows and the
  pause call for.
- **`RecentBlock` stays a transient overlay** that `grpc_server` sets
  directly. It follows an `AskRule`, so the daemon is up. Its revert is a
  resync, so it now respects `DaemonDown` too.
- **Files.** `grpc_server.rs` and `bridge-cli/src/lib.rs` aren't touched
  (PR #76); they already resync through the cache.

## Tests first

1. **Priority table.** Every combination of down, paused and
   pending (0 or n).
2. **While the daemon is down,** the tray stays `DaemonDown` through:
   - inserting a prompt;
   - resolving one;
   - cancelling one;
   - a recent-block revert;
   - a pause starting, and a pause expiring or being resumed.
3. **On recovery** the tray shows Pending(n), FilterOff or Idle. The
   existing watchdog tests cover this.
4. **Mutations:**
   - drop the daemon-down arm;
   - swap the pause and pending order;
   - have the watchdog stop setting the flag.
