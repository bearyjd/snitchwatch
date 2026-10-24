//! Exercises the real health model and page for both supported process-monitor
//! methods, unknown health, daemon monitor warnings, and firewall failure.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::daemon_health_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn daemon_health_model_registers_and_applies_report_json() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell
import "qrc:/qt/qml/com/snitchwatch/shell/qml" as Shell
Window {
    id: root
    visible: true
    width: 640
    height: 480
    property DaemonHealthModel model: DaemonHealthModel {}
    Shell.DaemonHealthPage { id: page; model: root.model }
    function find(item, name) {
        if (item.objectName === name) return item;
        for (let child of item.children) {
            const result = find(child, name);
            if (result) return result;
        }
        return null;
    }
    function check(condition, message) { if (!condition) throw new Error(message); }
    function report(monitor, firewall) {
        model.applyServerMessageJson(JSON.stringify({ action: "diagnosticsReport", checks: [
            { kind: "daemon_reachable", status: { status: "ok" } },
            { kind: "firewall_running", status: firewall },
            { kind: "nftables_support", status: { status: "ok" } },
            { kind: "ebpf_support", status: monitor }
        ] }));
    }
    Component.onCompleted: {
        check(model.statusSummary.indexOf("Waiting") >= 0, "initial state claimed healthy");
        const status = find(page, "daemonHealthStatus");
        const support = find(page, "processMonitorSupport");
        const details = find(page, "daemonHealthTroubleshooting");
        check(status && support && details, "health page labels missing");
        check(support.text.indexOf("/proc and eBPF") >= 0, "both methods not documented");
        check(support.text.indexOf("does not report the active method") >= 0, "active method guessed");
        // Current bridge cannot report active method. This is healthy for both
        // /proc and eBPF, including daemon fallback from eBPF to /proc.
        report({ status: "unknown" }, { status: "ok" });
        check(!model.hasProblem && status.text.indexOf("checks passed") >= 0, "unknown monitor treated as broken");
        check(!details.visible && support.visible, "healthy content visibility wrong");
        // Older bridge reports may positively identify eBPF support.
        report({ status: "ok" }, { status: "ok" });
        check(!model.hasProblem && status.text.indexOf("checks passed") >= 0, "eBPF compatibility regressed");
        report({ status: "unknown" }, { status: "unknown" });
        check(!model.hasProblem && status.text.indexOf("Waiting") >= 0, "unknown firewall claimed healthy");
        report({ status: "failed", detail: "warning: process monitor fallback" }, { status: "ok" });
        check(model.hasProblem && details.text.indexOf("warning: process monitor fallback") >= 0, "monitor warning lost");
        report({ status: "unknown" }, { status: "failed", detail: "firewall stopped" });
        check(model.hasProblem && details.text === "firewall stopped", "firewall failure hidden");
        console.info("DUAL_MODE_HEALTH_OK");
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
                &QUrl::from("qrc:/inline_daemon_health_probe.qml"),
            );
        }
    });
    drop(guard);
    assert!(
        stderr.contains("DUAL_MODE_HEALTH_OK"),
        "health page state checks did not complete: {stderr}"
    );

    assert!(
        root_ok.load(Ordering::SeqCst),
        "DaemonHealthModel QML probe failed: root object was null — the type failed to register \
         as a QML element or did not compile"
    );

    let _ = app.as_mut();
}
