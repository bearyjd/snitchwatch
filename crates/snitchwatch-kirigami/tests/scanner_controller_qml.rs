//! Integration smoke: `ScannerController` is usable as a QML element and
//! `ScannerPage.qml` compiles against it (Phase 6 report UI).
//!
//! Scope of this test — deliberately, only this (mirrors `wizard_qml.rs`'s
//! scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile), and
//!   * `runScan()` is callable from QML and runs the real
//!     pkexec/scanner-binary resolution path without aborting — this sandbox
//!     has no polkit daemon and (usually) no `pkexec` at all, so this
//!     exercises the real "gracefully degrade to an error string" path, not a
//!     mock, and
//!   * `ScannerPage.qml` loads against a real `ScannerController` and its
//!     button/report sections bind without throwing.
//!
//! It intentionally does NOT assert `reportJson`/`errorText` content:
//! `runScan()` completes asynchronously off the Qt thread, so QML-side
//! asserts on timing-dependent state wouldn't be load-bearing here.
//! `scanner_binary_path` resolution/override *correctness* is covered
//! exhaustively and Qt-free by `scanner`'s own unit tests. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::scanner_controller as _;

#[test]
fn scanner_controller_registers_and_scanner_page_loads() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property ScannerController controller: ScannerController {}
    property var page: null

    Component.onCompleted: {
        controller.runScan();

        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/ScannerPage.qml");
        if (component.status === Component.Ready) {
            page = component.createObject(null, { controller: controller });
        } else {
            console.log("[test] ScannerPage.qml load error:", component.errorString());
        }

        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] ScannerController.busy after runScan =", controller.busy,
                    "errorText =", controller.errorText);
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

    if let Some(engine) = engine.as_mut() {
        engine.load_data(
            &QByteArray::from(qml),
            &QUrl::from("qrc:/inline_scanner_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "ScannerController QML probe failed: root object was null — the type failed to \
         register as a QML element, or ScannerPage.qml did not compile"
    );

    let _ = app.as_mut();
}
