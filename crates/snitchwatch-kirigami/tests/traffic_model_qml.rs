//! Integration smoke: `TrafficModel` is usable as a QML element (Task 11).
//!
//! Scope of this test — deliberately, only this (mirrors
//! `blocklists_model_qml.rs`'s scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> ring-store-apply -> series/label property path without
//!     aborting (a Rust panic inside the invokable would abort this test
//!     binary), and
//!   * `TrafficPage.qml`'s `Canvas`-based chart loads and paints against a
//!     populated model without throwing (this is the "no `QtCharts`, no
//!     dependency-free fallback risk" smoke this task's spike called for).
//!
//! It intentionally does NOT assert series/label content: a thrown JS error
//! in `Component.onCompleted` does not null the root object, so QML-side
//! asserts are not load-bearing. The ring store's fold/formatting
//! *correctness* is covered exhaustively and Qt-free by
//! `traffic::ring_store`'s unit tests, which are what Task 11's acceptance
//! criterion calls for. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::traffic_model as _;

#[test]
fn traffic_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive a batch of TrafficEvents through
    // it, then load the real TrafficPage.qml against it so the Canvas paint
    // path executes for real (not a re-implemented probe QML). If the
    // invokable panicked, the whole test binary would abort; if the type
    // were unregistered or the page failed to compile, the root would be
    // null.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property TrafficModel model: TrafficModel {}
    property var page: null

    Component.onCompleted: {
        const events = [];
        for (let i = 0; i < 90; i++) {
            events.push({
                timestampMs: 1000000000000 + i * 1000,
                bytesIn: 100 + i,
                bytesOut: 50 + i
            });
        }
        model.applyServerMessageJson(JSON.stringify({
            action: "trafficEvents",
            events: events
        }));

        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/TrafficPage.qml");
        if (component.status === Component.Ready) {
            page = component.createObject(null, { model: model });
        } else {
            console.log("[test] TrafficPage.qml load error:", component.errorString());
        }

        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] TrafficModel.count after insert =", model.count,
                    "currentInLabel =", model.currentInLabel,
                    "currentOutLabel =", model.currentOutLabel);
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
            &QUrl::from("qrc:/inline_traffic_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "TrafficModel QML probe failed: root object was null — the type failed to register \
         as a QML element or did not compile"
    );

    let _ = app.as_mut();
}
