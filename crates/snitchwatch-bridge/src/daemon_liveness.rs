//! Daemon-liveness tracking for the bridge's gRPC `Ui` server.
//!
//! Split out of `grpc_server.rs` (which was growing past this repo's
//! 800-line file-size convention) so `daemon_watchdog` and `diagnostics`
//! can depend on this narrow type without importing from the gRPC service
//! module itself.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tracing::warn;

/// Tracks whether opensnitchd is alive from the bridge's point of view.
///
/// opensnitchd's poller only sends `Ping` when it has new stats events
/// (`vendor/opensnitch/daemon/ui/client.go:329`,
/// `daemon/statistics/stats.go:266`) — an idle daemon stays connected but
/// silent, so ping recency alone false-positives `DaemonDown`. Liveness is
/// therefore *any* inbound daemon-facing gRPC activity (`last_activity`),
/// with the long-lived `Notifications` stream's open/closed state as the
/// authoritative signal: an open stream is proof of life regardless of
/// `last_activity` staleness — see [`Self::is_down`].
///
/// All atomic operations here use `Ordering::SeqCst` rather than a more
/// relaxed ordering. This is a deliberate simplicity choice, not a
/// performance one: `is_down` is only ever polled from `daemon_watchdog`'s
/// 2-second-tick loop, nowhere near hot-path traffic, so the extra
/// sequential-consistency cost is immaterial and not worth reasoning about
/// weaker orderings for.
#[derive(Clone)]
pub struct DaemonLiveness {
    last_activity: Arc<StdMutex<Instant>>,
    open_notification_streams: Arc<AtomicUsize>,
}

impl DaemonLiveness {
    pub fn new() -> Self {
        Self {
            last_activity: Arc::new(StdMutex::new(Instant::now())),
            open_notification_streams: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Record inbound gRPC activity from the daemon. Called by every
    /// daemon-facing handler (`ping`, `ask_rule`, `subscribe`, `post_alert`,
    /// `notifications` open + each inbound reply).
    pub fn touch(&self) {
        *self.last_activity.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// A `Notifications` stream just opened. Also counts as activity.
    /// `pub(crate)` (not private) so `diagnostics::tests` can exercise the
    /// "stream open despite stale activity" scenario directly, and so
    /// [`StreamGuard`] (this module) can call it.
    pub(crate) fn open_notification_stream(&self) {
        self.open_notification_streams
            .fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    /// The daemon's side of a `Notifications` stream closed (its reply loop
    /// ended). Uses `fetch_update` with a saturating subtraction rather than
    /// a bare `fetch_sub`: an unbalanced call (closing more streams than
    /// were opened) would otherwise silently underflow `usize` to
    /// `usize::MAX` in a release build, wedging `is_down` permanently
    /// "alive". `StreamGuard` normally makes open/close pairing automatic,
    /// but this stays defense-in-depth for any other caller.
    pub(crate) fn close_notification_stream(&self) {
        let previous = self
            .open_notification_streams
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_sub(1))
            })
            .expect("closure always returns Some, so fetch_update always succeeds");
        if previous == 0 {
            warn!(
                "DaemonLiveness::close_notification_stream called with no open \
                 streams (already 0) — an open/close call pair is unbalanced; \
                 ignoring rather than underflowing the counter"
            );
        }
    }

    /// Down iff no `Notifications` stream is open AND `last_activity` is
    /// older than `timeout`. An open stream means alive regardless of
    /// staleness — exactly the idle-daemon shape a real opensnitchd
    /// produces. The staleness fallback covers a daemon that never opens
    /// the stream at all.
    pub fn is_down(&self, now: Instant, timeout: Duration) -> bool {
        if self.open_notification_streams.load(Ordering::SeqCst) > 0 {
            return false;
        }
        let last = *self.last_activity.lock().unwrap_or_else(|e| e.into_inner());
        now.duration_since(last) > timeout
    }

    /// Test-only: construct with `last_activity` set to `now - age`. Takes
    /// `now` explicitly (rather than calling `Instant::now()` internally)
    /// so callers can reuse the exact same instant they pass to
    /// [`Self::is_down`] — avoiding sub-millisecond drift between two
    /// separate `Instant::now()` calls that would make boundary-condition
    /// tests flaky. Lets plain synchronous unit tests (no tokio runtime /
    /// paused clock) exercise the staleness branch of [`Self::is_down`]
    /// without needing async time control.
    #[cfg(test)]
    pub(crate) fn new_stale_for_test(now: Instant, age: Duration) -> Self {
        Self {
            last_activity: Arc::new(StdMutex::new(now - age)),
            open_notification_streams: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Default for DaemonLiveness {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard around a single `Notifications` stream's open/close pair.
///
/// `grpc_server::UiService::notifications` used to call
/// `open_notification_stream()`/`close_notification_stream()` directly, with
/// the close call only reached at the bottom of the spawned reply-reading
/// loop. A panic partway through that loop (or any other early exit added
/// later) would skip the close, permanently wedging `open_notification_streams`
/// above zero and making `is_down` report the daemon alive forever. Moving
/// the counter into a guard's `Drop` makes the decrement run on every exit
/// path — normal loop end, early return, or unwind — because `Drop` runs
/// during unwinding.
pub(crate) struct StreamGuard {
    liveness: DaemonLiveness,
}

impl StreamGuard {
    /// Opens the stream (increments the counter, touches activity) and
    /// returns a guard whose `Drop` closes it again. Move the guard into
    /// whatever task/scope holds the stream open — dropping it (including
    /// via a panic unwind) is the only way the stream should ever be
    /// considered closed.
    pub(crate) fn open(liveness: DaemonLiveness) -> Self {
        liveness.open_notification_stream();
        Self { liveness }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.liveness.close_notification_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10x the ~1s ping cadence — mirrors `daemon_watchdog::DAEMON_DOWN_TIMEOUT`
    /// without depending on that module, so this file's tests don't need a
    /// cross-module import for a constant value.
    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn fresh_activity_is_alive() {
        let liveness = DaemonLiveness::new();
        assert!(!liveness.is_down(Instant::now(), TEST_TIMEOUT));
    }

    #[test]
    fn stale_and_no_stream_is_down() {
        let now = Instant::now();
        let liveness =
            DaemonLiveness::new_stale_for_test(now, TEST_TIMEOUT + Duration::from_secs(1));
        assert!(liveness.is_down(now, TEST_TIMEOUT));
    }

    #[test]
    fn stale_but_stream_open_is_alive() {
        let now = Instant::now();
        let liveness =
            DaemonLiveness::new_stale_for_test(now, TEST_TIMEOUT + Duration::from_secs(1));
        liveness.open_notification_stream();
        assert!(!liveness.is_down(now, TEST_TIMEOUT));
    }

    #[test]
    fn closing_the_only_open_stream_restores_staleness_check() {
        // Opening touches `last_activity`, so right after open+close it's
        // still fresh — not yet down.
        let opened_at = Instant::now();
        let liveness = DaemonLiveness::new();
        liveness.open_notification_stream();
        liveness.close_notification_stream();
        assert!(!liveness.is_down(opened_at, TEST_TIMEOUT));

        // Once that same (now-closed) activity ages past the timeout with
        // no stream open and no further activity, the staleness check
        // applies again.
        let later = opened_at + TEST_TIMEOUT + Duration::from_secs(1);
        assert!(liveness.is_down(later, TEST_TIMEOUT));
    }

    #[test]
    fn not_down_exactly_at_the_boundary() {
        let now = Instant::now();
        let liveness = DaemonLiveness::new_stale_for_test(now, TEST_TIMEOUT);
        // Strictly-greater-than semantics: exactly-at-timeout is not yet down.
        assert!(!liveness.is_down(now, TEST_TIMEOUT));
    }

    #[test]
    fn closing_with_no_open_stream_does_not_underflow() {
        // MED-1: an unbalanced close (no matching open) must saturate at 0,
        // not wrap to usize::MAX and wedge is_down alive forever.
        let now = Instant::now();
        let liveness =
            DaemonLiveness::new_stale_for_test(now, TEST_TIMEOUT + Duration::from_secs(1));
        liveness.close_notification_stream();
        assert!(
            liveness.is_down(now, TEST_TIMEOUT),
            "an unbalanced close must not leave the stream counter looking open"
        );
    }

    #[test]
    fn stream_guard_drop_closes_the_stream() {
        // `StreamGuard::open` calls `touch()`, which stamps `last_activity`
        // to the *real* current instant — so asserting staleness against a
        // `now` captured before opening would saturate to zero once
        // touched. Use a `far_future` reference point well past the
        // timeout instead: it stays past the (real, post-touch)
        // `last_activity` no matter when `touch()` actually ran, as long as
        // the test itself takes nowhere near `TEST_TIMEOUT` to execute.
        let liveness = DaemonLiveness::new();
        let far_future = Instant::now() + TEST_TIMEOUT + Duration::from_secs(1);

        let guard = StreamGuard::open(liveness.clone());
        assert!(
            !liveness.is_down(far_future, TEST_TIMEOUT),
            "guard holds it open"
        );

        drop(guard);
        assert!(
            liveness.is_down(far_future, TEST_TIMEOUT),
            "dropping the guard must close the stream"
        );
    }

    #[test]
    fn stream_guard_closes_on_panic_unwind() {
        // HIGH-2's actual guarantee: a panic partway through the guarded
        // scope still runs Drop, so the stream isn't wedged open forever.
        // Same `far_future` reasoning as the test above.
        let liveness = DaemonLiveness::new();
        let far_future = Instant::now() + TEST_TIMEOUT + Duration::from_secs(1);

        let liveness_for_panic = liveness.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = StreamGuard::open(liveness_for_panic.clone());
            panic!("simulated failure inside the guarded scope");
        }));
        assert!(result.is_err());

        assert!(
            liveness.is_down(far_future, TEST_TIMEOUT),
            "the guard's Drop must have run during the panic's unwind"
        );
    }
}
