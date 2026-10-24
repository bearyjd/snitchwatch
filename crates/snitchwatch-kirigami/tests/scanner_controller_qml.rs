//! Register the real controller and instantiate its page without launching a scan.
//! Method presence is checked in QML; scanner execution belongs to broker tests.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::scanner_controller as _;
mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn scanner_controller_registers_and_scanner_page_loads() {
    init_headless_qt_env();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    visible: true
    width: 900; height: 700
    ScannerController { id: controller }
    property var page: null

    Component.onCompleted: {
        if (typeof controller.runScan !== "function") throw new Error("runScan is not registered");
        if (controller.busy || controller.errorText !== "" || controller.reportJson !== "")
            throw new Error("controller initial state is invalid");
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/ScannerPage.qml");
        if (component.status !== Component.Ready) throw new Error(component.errorString());
        page = component.createObject(contentItem, { controller: controller });
        if (!page || page.controller !== controller) throw new Error("scanner page did not bind controller");
        console.warn("SCANNER_REGISTRATION_COMPLETE");
        Qt.callLater(Qt.quit);
    }
    Timer {
        interval: 5000; running: true
        onTriggered: { console.warn("SCANNER_REGISTRATION_TIMEOUT"); Qt.quit(); }
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
                &QUrl::from("qrc:/inline_scanner_probe.qml"),
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
        "scanner registration failed: {captured}"
    );
    assert!(
        captured.contains("SCANNER_REGISTRATION_COMPLETE"),
        "scanner page was not created: {captured}"
    );
    assert!(
        !captured.contains("Error:") && !captured.contains("SCANNER_REGISTRATION_TIMEOUT"),
        "{captured}"
    );
}
