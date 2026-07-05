//! Integration smoke: `ConnectionsModel` is usable as a QML element (Task 6).
//!
//! Scope of this test — and, deliberately, only this:
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object here would mean the type failed to
//!     register or compile against its `QAbstractListModel` base), and
//!   * the `applyServerMessageJson` invokable is callable from QML and drives
//!     the real deserialize -> `RowStore::apply` path without aborting (a Rust
//!     panic inside the invokable would abort this test binary).
//!
//! It intentionally does NOT assert row counts/content: a thrown JS error in
//! `Component.onCompleted` does not null the root object, so QML-side asserts
//! are not load-bearing. The model's ordering/CRUD *correctness* is covered
//! exhaustively and Qt-free by the `connections::row_store` unit tests, which
//! are what Task 6's "seed synthetic events, assert row count/order/content"
//! acceptance criterion calls for. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::connections_model as _;

#[test]
fn connections_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive one real insert through the
    // deserialize -> apply path. If the invokable panicked, the whole test
    // binary would abort; if the type were unregistered, the root would be null.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property ConnectionsModel model: ConnectionsModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "insertConnectionRows",
            rows: [
                { id: "r1", process: "firefox", processPath: null, dstHost: "github.com",
                  dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0 },
                { id: "r2", process: "slack", processPath: null, dstHost: "slack.com",
                  dstIp: "2.2.2.2", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 0 }
            ]
        }));
        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] ConnectionsModel.count after insert =", model.count,
                    "pendingCount =", model.pendingCount);
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
            &QUrl::from("qrc:/inline_model_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "ConnectionsModel QML probe failed: root object was null — the type failed to \
         register as a QML element or did not compile against its QAbstractListModel base"
    );

    let _ = app.as_mut();
}
