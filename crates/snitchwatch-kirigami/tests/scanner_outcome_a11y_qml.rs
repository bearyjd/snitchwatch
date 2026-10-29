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
    property int step: 0
    property var page: null
    ScannerController { id: scanner }
    Item { id: holder; anchors.fill: parent }
    Component {
        id: pageComponent
        ScannerPage {
            controller: scanner
            onOutcomeAnnounced: message => root.announced = root.announced.concat([message])
        }
    }
    // Same shape as StackNav.createPage: built parentless, attached afterwards.
    function createParentless() { root.page = pageComponent.createObject(null, {}); }
    function attach() { root.page.parent = holder; }
    function drop() { if (root.page) { root.page.destroy(); root.page = null; } }
    function find(item, name) {
        if (item.objectName === name) return item;
        for (let child of item.children) { let result = find(child, name); if (result) return result; }
        return null;
    }
    function expect(ok, what) { if (!ok) throw new Error(what + " (announced=" + JSON.stringify(root.announced) + ")"); }
    function finish(error) {
        console.warn(error ? "SCANNER_A11Y_FAILED: " + error : "SCANNER_A11Y_COMPLETE");
        Qt.quit();
    }
    // Each step starts real scans (missing-binary preflight failure) and
    // continues once the worker has reported back.
    function advance() {
        try {
            if (root.step === 0) {
                // (a) the page goes away mid-scan; the scan finishes without it
                createParentless(); attach();
                expect(root.announced.length === 0, "announced before any scan");
                scanner.runScan();
                drop();
                root.step = 1;
            } else if (root.step === 1) {
                expect(root.announced.length === 0, "announced with no page alive");
                expect(scanner.summaryText.length > 0, "no outcome");
                const outcome = scanner.summaryText;
                createParentless();
                expect(root.announced.length === 0, "announced from a detached page");
                attach();
                expect(root.announced.length === 1 && root.announced[0] === outcome,
                       "outcome finished off-page was not announced exactly once on attach");
                // (b) later revisits do not repeat a stale summary
                drop(); createParentless(); attach();
                drop(); createParentless(); attach();
                expect(root.announced.length === 1, "stale summary re-announced on revisit");
                // (c) page alive: the busy -> false path announces once
                const banner = root.find(root.page, "scannerSummary");
                expect(!!banner, "summary banner missing");
                scanner.runScan();
                expect(banner.Accessible.name === "Scanning…", "busy banner name is '" + banner.Accessible.name + "'");
                expect(root.announced.length === 1, "announced while busy");
                root.step = 2;
            } else if (root.step === 2) {
                const banner = root.find(root.page, "scannerSummary");
                expect(root.announced.length === 2 && root.announced[1] === scanner.summaryText,
                       "live completion not announced exactly once");
                expect(banner.Accessible.name === scanner.summaryText,
                       "outcome banner name is '" + banner.Accessible.name + "'");
                // An unannounced outcome, then a new scan: the old one is dropped.
                drop();
                scanner.runScan();
                root.step = 3;
            } else if (root.step === 3) {
                scanner.runScan();
                createParentless(); attach();
                expect(scanner.busy, "second scan should be running");
                expect(root.announced.length === 2, "old outcome announced while a new scan is busy");
                root.step = 4;
            } else {
                expect(root.announced.length === 3 && root.announced[2] === scanner.summaryText,
                       "new scan's outcome not announced exactly once");
                finish(null);
                return;
            }
            waitForScan.start();
        } catch (error) { finish(error); }
    }
    Timer {
        id: waitForScan
        interval: 50; repeat: true
        onTriggered: if (!scanner.busy) { stop(); root.advance(); }
    }
    Timer {
        interval: 15000; running: true
        onTriggered: { console.warn("SCANNER_A11Y_TIMEOUT"); Qt.quit(); }
    }
    Timer {
        interval: 200; running: true
        onTriggered: root.advance()
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
