//! Flicker hypothesis (owner sees a one-line "waiting"-like text for a few ms):
//! `DaemonHealthModel` passes through `SUMMARY_PENDING` ("Waiting for complete
//! daemon health information") between two complete states. This records every
//! change signal of the user-visible properties while feeding the real model
//! sequences of reports, so an intermediate value is observable even if it only
//! lives for one call (signals are synchronous: a binding sees each one before
//! any frame can be drawn).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::daemon_health_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn health_summary_never_passes_through_pending_between_complete_reports() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: root
    visible: false
    property DaemonHealthModel model: DaemonHealthModel {}
    property var log: []
    Connections {
        target: root.model
        function onStatusSummaryChanged() { root.log.push("summary=" + root.model.statusSummary); }
        function onHasProblemChanged() { root.log.push("hasProblem=" + root.model.hasProblem); }
        function onDaemonDownChanged() { root.log.push("daemonDown=" + root.model.daemonDown); }
    }
    Timer { interval: 45000; running: true; onTriggered: { console.error("WATCHDOG"); Qt.quit(); } }
    function check(c, m) { if (!c) throw new Error(m); }
    function ok() { return { status: "ok" }; }
    function failed() { return { status: "failed", detail: "x" }; }
    function unknown() { return { status: "unknown" }; }
    function report(daemon, firewall, nft) {
        model.applyServerMessageJson(JSON.stringify({ action: "diagnosticsReport", checks: [
            { kind: "daemon_reachable", status: daemon },
            { kind: "firewall_running", status: firewall },
            { kind: "nftables_support", status: nft },
            { kind: "ebpf_support", status: unknown() }
        ] }));
    }
    // Run `step`, return the signals it emitted (and clear the log).
    function recorded(step) { log = []; step(); const out = log; log = []; return out; }
    function noPending(entries, when) {
        for (const e of entries)
            check(e.indexOf("Waiting") < 0, "pending text was emitted " + when + ": " + entries.join(" | "));
    }
    Component.onCompleted: {
        // partial -> complete: a pending report keeps the initial pending text
        // (no emission), the complete one flips once.
        let seen = recorded(() => report(ok(), unknown(), ok()));
        check(seen.length === 0, "partial report emitted " + seen.join(" | "));
        seen = recorded(() => report(ok(), ok(), ok()));
        check(seen.length === 1 && seen[0].indexOf("checks passed") >= 0, "partial->complete: " + seen.join(" | "));

        // complete -> complete (identical): nothing at all may be emitted.
        seen = recorded(() => report(ok(), ok(), ok()));
        check(seen.length === 0, "identical complete report emitted " + seen.join(" | "));

        // complete -> daemon down -> recovery (complete): exactly one summary
        // change each way, never pending, and never a torn flag/summary pair
        // that outlives the call.
        seen = recorded(() => report(failed(), unknown(), ok()));
        noPending(seen, "going down");
        check(model.hasProblem && model.daemonDown, "down flags");
        seen = recorded(() => report(ok(), ok(), ok()));
        noPending(seen, "on recovery");
        check(seen.filter(e => e.indexOf("summary=") === 0).length === 1, "recovery summary emitted != once: " + seen.join(" | "));
        check(!model.hasProblem && !model.daemonDown && model.statusSummary.indexOf("checks passed") >= 0, "recovered state");

        // complete -> problem -> complete: same.
        seen = recorded(() => report(ok(), failed(), ok()));
        noPending(seen, "firewall failing");
        seen = recorded(() => report(ok(), ok(), ok()));
        noPending(seen, "firewall recovering");

        // Characterisation of the real source of "Waiting": a report whose
        // firewall check is unknown (the bridge sends exactly this when the
        // daemon comes back before its Subscribe arrives). The model must show
        // it faithfully; the flicker would then come from the bridge.
        seen = recorded(() => report(ok(), unknown(), ok()));
        check(seen.length === 1 && seen[0].indexOf("Waiting") >= 0, "unknown firewall: " + seen.join(" | "));
        console.info("HEALTH_TRANSIENT_OK");
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let stderr = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(qml),
                &QUrl::from("qrc:/inline_health_transient_probe.qml"),
            );
        }
    });
    drop(guard);
    assert!(
        stderr.contains("HEALTH_TRANSIENT_OK"),
        "health transient checks did not complete: {stderr}"
    );
    assert!(root_ok.load(Ordering::SeqCst), "probe root was null");
    let _ = app.as_mut();
}
