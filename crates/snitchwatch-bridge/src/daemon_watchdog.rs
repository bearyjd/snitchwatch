//! Daemon-down detection off `ping()`'s recency.
//!
//! opensnitchd's own poller (`vendor/opensnitch/daemon/ui/client.go`'s
//! `poller()` loop) connects-or-pings roughly once per second while
//! connected — a read-only fact confirmed from the vendored submodule, not
//! something this repo controls. `grpc_server.rs`'s `ping()` handler
//! records each arrival's timestamp; this module watches that timestamp for
//! staleness and republishes `TrayState::DaemonDown` (or, on recovery,
//! whatever `Idle`/`Pending(n)` the cache actually holds).

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Mutex as TokioMutex};

use crate::cache::connections::ConnectionCache;
use crate::tray_state::{TrayState, TrayStatePublisher};

/// 10x the ~1s ping cadence observed in `vendor/opensnitch/daemon/ui/client.go`
/// — generous enough to absorb a dropped ping or scheduling jitter without
/// false-positiving, tight enough to notice a real outage quickly. An
/// assumption documented here, not a measured SLA.
pub const DAEMON_DOWN_TIMEOUT: Duration = Duration::from_secs(10);

const WATCHDOG_TICK: Duration = Duration::from_secs(2);

/// Pure staleness check: has it been longer than `timeout` since `last_ping`?
pub fn is_daemon_down(last_ping: Instant, now: Instant, timeout: Duration) -> bool {
    now.duration_since(last_ping) > timeout
}

/// Poll `last_ping` every [`WATCHDOG_TICK`] and keep the tray's `DaemonDown`
/// state in sync with it. Runs until the task is dropped/aborted by its
/// caller (mirrors `ws_server`/`grpc` serve tasks' own lifetime — owned by
/// `snitchwatch-bridge-cli::run`'s `RunningBridge`, not self-terminating).
pub async fn run(
    last_ping: Arc<StdMutex<Instant>>,
    tray_pub: Arc<TrayStatePublisher>,
    cache: Arc<TokioMutex<ConnectionCache>>,
    diagnostics_ctx: Arc<crate::diagnostics::DiagnosticsCtx>,
    broadcast_tx: broadcast::Sender<crate::ws_messages::ServerMessage>,
) {
    let mut interval = tokio::time::interval(WATCHDOG_TICK);
    let mut was_down = false;
    loop {
        interval.tick().await;

        let last_ping_ts = {
            let guard = last_ping.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let down_now = is_daemon_down(last_ping_ts, Instant::now(), DAEMON_DOWN_TIMEOUT);

        if down_now && !was_down {
            tray_pub.set(TrayState::DaemonDown);
            let _ = broadcast_tx.send(crate::ws_messages::ServerMessage::DiagnosticsReport {
                checks: diagnostics_ctx.report(),
            });
        } else if !down_now && was_down {
            // Recovered — show what the cache actually holds, not a
            // hardcoded Idle (there may be pending rows queued up from
            // before the outage, or new ones that arrived while "down").
            cache.lock().await.resync_tray_state();
            let _ = broadcast_tx.send(crate::ws_messages::ServerMessage::DiagnosticsReport {
                checks: diagnostics_ctx.report(),
            });
        }
        was_down = down_now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_down_when_ping_is_recent() {
        let now = Instant::now();
        let last_ping = now - Duration::from_secs(1);
        assert!(!is_daemon_down(last_ping, now, DAEMON_DOWN_TIMEOUT));
    }

    #[test]
    fn down_when_ping_older_than_timeout() {
        let now = Instant::now();
        let last_ping = now - Duration::from_secs(11);
        assert!(is_daemon_down(last_ping, now, DAEMON_DOWN_TIMEOUT));
    }

    #[test]
    fn not_down_exactly_at_the_boundary() {
        let now = Instant::now();
        let last_ping = now - DAEMON_DOWN_TIMEOUT;
        // Strictly-greater-than semantics: exactly-at-timeout is not yet down.
        assert!(!is_daemon_down(last_ping, now, DAEMON_DOWN_TIMEOUT));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_reports_down_then_recovers_to_cache_state() {
        let last_ping = Arc::new(StdMutex::new(Instant::now()));
        let tray_pub = Arc::new(TrayStatePublisher::new());
        let cache = Arc::new(TokioMutex::new(ConnectionCache::with_tray_publisher(
            64,
            tray_pub.clone(),
        )));
        let mut rx = tray_pub.subscribe();

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn crate::diagnostics::kernel_probe::KernelProbe> =
            Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok());
        let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
            last_ping.clone(),
            firewall_status,
            probe,
        ));

        let watchdog = tokio::spawn(run(
            last_ping.clone(),
            tray_pub.clone(),
            cache.clone(),
            diagnostics_ctx,
            broadcast_tx,
        ));

        // No ping updates arrive; advance past the timeout.
        tokio::time::advance(DAEMON_DOWN_TIMEOUT + Duration::from_secs(1)).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);

        // "Ping" resumes.
        *last_ping.lock().unwrap() = Instant::now();
        tokio::time::advance(WATCHDOG_TICK * 2).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Idle);

        watchdog.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_recovers_to_pending_when_rows_are_queued() {
        let last_ping = Arc::new(StdMutex::new(Instant::now()));
        let tray_pub = Arc::new(TrayStatePublisher::new());
        let cache = Arc::new(TokioMutex::new(ConnectionCache::with_tray_publisher(
            64,
            tray_pub.clone(),
        )));
        let mut rx = tray_pub.subscribe();

        // Queue a pending row before the daemon "goes down".
        let row = crate::ws_messages::ConnectionRow {
            id: "1".to_string(),
            process: "firefox".to_string(),
            process_path: None,
            dst_host: "example.com".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
        };
        let _verdict_rx = cache.lock().await.insert_pending(row);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(1));

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn crate::diagnostics::kernel_probe::KernelProbe> =
            Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok());
        let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
            last_ping.clone(),
            firewall_status,
            probe,
        ));

        let watchdog = tokio::spawn(run(
            last_ping.clone(),
            tray_pub.clone(),
            cache.clone(),
            diagnostics_ctx,
            broadcast_tx,
        ));

        tokio::time::advance(DAEMON_DOWN_TIMEOUT + Duration::from_secs(1)).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::DaemonDown);

        *last_ping.lock().unwrap() = Instant::now();
        tokio::time::advance(WATCHDOG_TICK * 2).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), TrayState::Pending(1));

        watchdog.abort();
    }

    #[tokio::test]
    async fn watchdog_broadcasts_diagnostics_report_on_down_transition() {
        let stale = Instant::now() - (DAEMON_DOWN_TIMEOUT + Duration::from_secs(1));
        let last_ping = Arc::new(StdMutex::new(stale));
        let tray_pub = Arc::new(TrayStatePublisher::new());
        let cache = Arc::new(TokioMutex::new(ConnectionCache::new(64)));
        let (broadcast_tx, mut broadcast_rx) = tokio::sync::broadcast::channel(16);
        let firewall_status = Arc::new(StdMutex::new(None));
        let probe: Arc<dyn crate::diagnostics::kernel_probe::KernelProbe> =
            Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok());
        let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
            last_ping.clone(),
            firewall_status,
            probe,
        ));

        let handle = tokio::spawn(run(
            last_ping,
            tray_pub,
            cache,
            diagnostics_ctx,
            broadcast_tx,
        ));

        let msg = tokio::time::timeout(Duration::from_secs(3), broadcast_rx.recv())
            .await
            .expect("timed out waiting for DiagnosticsReport")
            .unwrap();
        assert!(matches!(
            msg,
            crate::ws_messages::ServerMessage::DiagnosticsReport { .. }
        ));

        handle.abort();
    }
}
