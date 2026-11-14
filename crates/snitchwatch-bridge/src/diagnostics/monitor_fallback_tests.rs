//! E3: a daemon alert saying eBPF fell back to `/proc` is reported as the
//! process monitor's state, never as a failed check.

use super::kernel_probe::testing::FakeKernelProbe;
use super::*;
use crate::daemon_alerts::tests::{DNS_MODULE_ALERT, MAIN_MODULE_FALLBACK, PROCS_MODULE_FALLBACK};
use crate::ws_messages::MonitorFallback;

fn ctx_with(alerts: &[&str]) -> DiagnosticsCtx {
    let store = Arc::new(DaemonAlertStore::new());
    for text in alerts {
        store.record(
            alert::What::Generic,
            alert::Type::Warning as i32,
            text.to_string(),
        );
    }
    DiagnosticsCtx::new(
        DaemonLiveness::new(),
        Arc::new(StdMutex::new(Some(true))),
        Arc::new(FakeKernelProbe::all_ok()),
        store,
    )
}

fn ebpf_status(ctx: &DiagnosticsCtx) -> CheckStatus {
    ctx.report()
        .into_iter()
        .find(|c| c.kind == CheckKind::EbpfSupport)
        .unwrap()
        .status
}

#[test]
fn main_module_fallback_is_reported_and_not_a_failure() {
    let ctx = ctx_with(&[MAIN_MODULE_FALLBACK, DNS_MODULE_ALERT]);
    let report = ctx.process_monitor().expect("fallback lost");
    assert_eq!(report.fallback, MonitorFallback::EbpfDidNotStart);
    assert_eq!(report.daemon_text, MAIN_MODULE_FALLBACK);
    assert!(report.reported_secs_ago < 5);
    assert_eq!(ebpf_status(&ctx), CheckStatus::Unknown);
}

#[test]
fn procs_module_fallback_is_reported() {
    let ctx = ctx_with(&[PROCS_MODULE_FALLBACK]);
    assert_eq!(
        ctx.process_monitor().map(|r| r.fallback),
        Some(MonitorFallback::EbpfEventsDidNotStart)
    );
}

#[test]
fn no_or_unrecognised_alert_reports_nothing() {
    assert_eq!(ctx_with(&[]).process_monitor(), None);
    assert_eq!(
        ctx_with(&["ringbuf: not supported"]).process_monitor(),
        None
    );
    assert_eq!(ctx_with(&[DNS_MODULE_ALERT]).process_monitor(), None);
}

#[test]
fn dns_module_alert_does_not_fail_the_monitor_check() {
    assert_eq!(
        ebpf_status(&ctx_with(&[DNS_MODULE_ALERT])),
        CheckStatus::Unknown
    );
}

#[test]
fn recheck_clears_the_fallback() {
    let ctx = ctx_with(&[MAIN_MODULE_FALLBACK]);
    ctx.clear_alerts();
    assert_eq!(ctx.process_monitor(), None);
}

#[test]
fn no_fallback_is_reported_while_the_daemon_is_down() {
    let store = Arc::new(DaemonAlertStore::new());
    store.record(
        alert::What::Generic,
        alert::Type::Warning as i32,
        MAIN_MODULE_FALLBACK.to_string(),
    );
    let ctx = DiagnosticsCtx::new(
        DaemonLiveness::new_stale_for_test(
            std::time::Instant::now(),
            crate::daemon_watchdog::DAEMON_DOWN_TIMEOUT + std::time::Duration::from_secs(1),
        ),
        Arc::new(StdMutex::new(None)),
        Arc::new(FakeKernelProbe::all_ok()),
        store,
    );
    assert_eq!(ctx.process_monitor(), None);
}

#[test]
fn clearing_the_fallback_keeps_other_alerts() {
    let ctx = ctx_with(&[
        "Unable to set process monitor method via parameter: x",
        MAIN_MODULE_FALLBACK,
    ]);
    ctx.clear_monitor_fallback();
    assert_eq!(ctx.process_monitor(), None);
    assert!(matches!(ebpf_status(&ctx), CheckStatus::Failed { .. }));
}

/// E3 round 4: a clear scoped to an outage sampled earlier must not drop a
/// fallback a replacement daemon reported afterwards.
#[test]
fn a_scoped_clear_keeps_a_fallback_recorded_after_the_sample() {
    let sampled_at = Instant::now();
    let ctx = ctx_with(&[MAIN_MODULE_FALLBACK]);
    ctx.clear_monitor_fallback_recorded_before(sampled_at);
    assert!(ctx.process_monitor().is_some());
}

#[test]
fn a_scoped_clear_drops_a_fallback_recorded_before_the_sample() {
    let ctx = ctx_with(&[MAIN_MODULE_FALLBACK]);
    ctx.clear_monitor_fallback_recorded_before(Instant::now());
    assert_eq!(ctx.process_monitor(), None);
}

/// E3 round 4: the snapshot is taken under the publish lock, so a publish
/// that was waiting while another site cleared the fallback sends the
/// post-clear state, never a stale one.
#[test]
fn a_publish_blocked_behind_a_clear_sends_the_post_clear_state() {
    let ctx = Arc::new(ctx_with(&[MAIN_MODULE_FALLBACK]));
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    let guard = ctx.lock_publish_for_test();
    let publisher = {
        let ctx = ctx.clone();
        std::thread::spawn(move || ctx.publish(&tx))
    };
    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_recv().is_err(), "publish ran without the lock");
    ctx.clear_monitor_fallback();
    drop(guard);
    publisher.join().unwrap().unwrap();
    let crate::ws_messages::ServerMessage::DiagnosticsReport {
        process_monitor, ..
    } = rx.try_recv().unwrap()
    else {
        panic!("expected DiagnosticsReport");
    };
    assert_eq!(process_monitor, None);
}
