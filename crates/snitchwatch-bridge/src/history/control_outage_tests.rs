//! PR 10 review: a capture that a control operation puts in the slot during
//! a daemon outage must be told the daemon is already away, or its session
//! starts mid-outage with no `daemon-disconnected` gap.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::control::Limits;
use super::control_support::consent;
use super::control_tests::gate_rig;

#[tokio::test(start_paused = true)]
async fn a_capture_swapped_in_during_an_outage_is_told_the_daemon_is_down() {
    let rig = gate_rig(Limits::default());
    let first = rig.gate.slot().get().expect("recording");
    let liveness = crate::daemon_liveness::DaemonLiveness::new();
    liveness.touch();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(tokio::sync::Mutex::new(
        crate::cache::connections::ConnectionCache::with_tray_publisher(64, tray_pub.clone()),
    ));
    let mut rx = tray_pub.subscribe();
    let (broadcast_tx, _) = tokio::sync::broadcast::channel(16);
    let diagnostics_ctx = Arc::new(crate::diagnostics::DiagnosticsCtx::new(
        liveness.clone(),
        Arc::new(Mutex::new(None)),
        Arc::new(crate::diagnostics::kernel_probe::testing::FakeKernelProbe::all_ok()),
        Arc::new(crate::daemon_alerts::DaemonAlertStore::new()),
    ));
    let watchdog = tokio::spawn(crate::daemon_watchdog::run_with_history(
        liveness,
        tray_pub,
        cache,
        diagnostics_ctx,
        broadcast_tx,
        Some(rig.gate.slot()),
    ));
    tokio::time::advance(crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT + Duration::from_secs(1))
        .await;
    rx.changed().await.unwrap();
    assert!(first.daemon_gap_open_for_test(), "the outage reached it");

    // The user disables and enables again while the daemon is still away.
    rig.gate.disable().unwrap();
    rig.gate.enable(consent()).unwrap();
    let second = rig.gate.slot().get().expect("recording again");
    assert!(!Arc::ptr_eq(&first, &second));
    drop(first);

    tokio::time::advance(crate::daemon_watchdog::WATCHDOG_TICK * 2).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        second.daemon_gap_open_for_test(),
        "the new capture started inside the outage"
    );
    watchdog.abort();
}
