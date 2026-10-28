//! The scan outcome banner is announced to assistive tech when a scan ends,
//! and its alert role carries the banner text as its accessible name.
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
fn scan_outcome_banner_is_named_and_announced_when_busy_ends() {
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
    property var announced: []
    ScannerController { id: scanner }
    ScannerPage {
        id: page; anchors.fill: parent; controller: scanner
        onOutcomeAnnounced: message => root.announced = root.announced.concat([message])
    }
    function find(item, name) {
        if (item.objectName === name) return item;
        for (let child of item.children) { let result = find(child, name); if (result) return result; }
        return null;
    }
    Timer {
        interval: 5000; running: true
        onTriggered: { console.warn("SCANNER_A11Y_TIMEOUT"); Qt.quit(); }
    }
    Timer {
        interval: 200; running: true
        onTriggered: {
            try {
                const banner = root.find(page, "scannerSummary");
                if (!banner) throw new Error("summary banner missing");
                scanner.busy = true;
                if (banner.Accessible.name !== "Scanning…")
                    throw new Error("busy banner name is '" + banner.Accessible.name + "'");
                if (root.announced.length !== 0) throw new Error("announced before the scan ended");
                const outcome = "Scan finished at 12:00: 2 anomalies found.";
                scanner.summaryKind = "findings";
                scanner.summaryText = outcome;
                if (root.announced.length !== 0) throw new Error("announced while still busy");
                scanner.busy = false;
                if (banner.Accessible.name !== outcome)
                    throw new Error("outcome banner name is '" + banner.Accessible.name + "'");
                if (root.announced.length !== 1 || root.announced[0] !== outcome)
                    throw new Error("outcome not announced: " + JSON.stringify(root.announced));
                console.warn("SCANNER_A11Y_COMPLETE");
            } catch (error) {
                console.warn("SCANNER_A11Y_FAILED: " + error);
            } finally { Qt.quit(); }
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
                &QUrl::from("qrc:/scanner_a11y_probe.qml"),
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
        "scanner a11y probe failed to load: {captured}"
    );
    assert!(
        captured.contains("SCANNER_A11Y_COMPLETE"),
        "scanner a11y assertions did not complete: {captured}"
    );
    assert!(
        !captured.contains("SCANNER_A11Y_TIMEOUT")
            && !captured.contains("SCANNER_A11Y_FAILED")
            && !captured.contains("Error:")
            && !captured.contains("ReferenceError")
            && !captured.contains("TypeError"),
        "{captured}"
    );
}
