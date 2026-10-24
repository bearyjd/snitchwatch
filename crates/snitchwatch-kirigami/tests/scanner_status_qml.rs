//! Exercise typed scanner status and the in-flight guard without root or polkit.
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn scanner_setup_and_denial_are_distinct_and_busy_disables_run() {
    init_headless_qt_env();
    // A guard regression must fail the assertions without opening polkit.
    // This exercises the real missing-file preflight if a worker is started.
    let missing_scanner = tempfile::tempdir().expect("scanner preflight fixture");
    std::env::set_var(
        "SNITCHWATCH_SCANNER_BIN",
        missing_scanner.path().join("not-installed"),
    );
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell
Window {
    id: root
    visible: true
    width: 900; height: 700
    ScannerController { id: scanner }
    ScannerPage { id: page; anchors.fill: parent; controller: scanner }
    function find(item, name) {
        if (item.objectName === name) return item;
        for (let child of item.children) { let result = find(child, name); if (result) return result; }
        return null;
    }
    Timer {
        interval: 5000; running: true
        onTriggered: { console.warn("SCANNER_STATUS_TIMEOUT"); Qt.quit(); }
    }
    Timer {
        id: guardCheck
        interval: 100
        onTriggered: {
            try {
                if (!scanner.busy || scanner.errorTitle !== "The deep scan was not authorized."
                    || scanner.errorText !== "<b>missing host integration</b>" || scanner.reportJson !== "")
                    throw new Error("busy invocation changed state asynchronously");
                console.warn("SCANNER_STATUS_COMPLETE");
            } finally { Qt.quit(); }
        }
    }
    Timer {
        interval: 200; running: true
        onTriggered: {
            try {
                let message = root.find(page, "scannerStatusMessage");
                let detail = root.find(page, "scannerErrorText");
                let run = root.find(page, "scannerRunButton");
                if (!message || !detail || !run) throw new Error("scanner widgets missing");
                scanner.errorTitle = "Host scanner setup is required.";
                scanner.errorText = "<b>missing host integration</b>";
                if (message.text !== scanner.errorTitle || !message.visible) throw new Error("setup status missing");
                if (detail.textFormat !== Text.PlainText) throw new Error("scanner details allow markup");
                scanner.errorTitle = "The deep scan was not authorized.";
                if (message.text !== scanner.errorTitle) throw new Error("denial status missing");
                scanner.busy = true;
                if (run.enabled || run.text !== "Scanning…") throw new Error("busy scan can be restarted");
                scanner.runScan(); // Must return before spawning any worker/prompt.
                if (!scanner.busy || scanner.errorText.length === 0) throw new Error("busy guard changed state");
                guardCheck.start();
            } catch (error) {
                console.warn("SCANNER_STATUS_FAILED: " + error);
                Qt.quit();
            }
        }
    }
}
"#;
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_, obj, _| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(qml),
                &QUrl::from("qrc:/scanner_status_probe.qml"),
            );
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(
        root_ok.load(Ordering::SeqCst),
        "scanner probe failed to load: {captured}"
    );
    assert!(
        captured.contains("SCANNER_STATUS_COMPLETE"),
        "scanner assertions did not complete: {captured}"
    );
    assert!(
        !captured.contains("SCANNER_STATUS_TIMEOUT")
            && !captured.contains("SCANNER_STATUS_FAILED")
            && !captured.contains("Error:")
            && !captured.contains("ReferenceError")
            && !captured.contains("TypeError"),
        "{captured}"
    );
}
