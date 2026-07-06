//! Integration smoke: `ConnectionsPage.qml`'s rule-match diagnostics
//! additions — the matched-rule inspector fields and the `showRuleRequested`
//! signal that main.qml routes to `RulesPage.openRuleByName`.
//!
//! Scope of this test — same convention as `connections_model_qml.rs`:
//!   * `ConnectionsPage.qml` compiles and instantiates directly as a QML
//!     type under `com.snitchwatch.shell` (a null root object would mean a
//!     type/property error in the file), and
//!   * `openInspector` (with the new `matchedRule`/`matchedRuleDisplay`
//!     fields) and emitting `showRuleRequested` run without a Rust panic
//!     aborting the test binary.
//!
//! As with the existing model smokes, a thrown JS error inside
//! `Component.onCompleted` does not null the root object, so this does not
//! assert on the inspector's *content* — that correctness is covered
//! exhaustively and Qt-free by `connections::row_store::matched_rule_display`'s
//! unit tests. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[test]
fn connections_page_inspector_and_show_rule_signal_run_without_erroring() {
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

ConnectionsPage {
    id: page
    model: ConnectionsModel {
        Component.onCompleted: {
            applyServerMessageJson(JSON.stringify({
                action: "insertConnectionRows",
                rows: [
                    { id: "r1", process: "curl", processPath: null, dstHost: "github.com",
                      dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                      action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                      matchedRule: "899-curl-allow" }
                ]
            }));
        }
    }
    Component.onCompleted: {
        page.openInspector({
            rowId: "r1", process: "curl", host: "github.com", port: 443,
            protocol: "tcp", verdict: "allowed", pending: false,
            matchedRule: "899-curl-allow", matchedRuleDisplay: "899-curl-allow"
        });
        page.showRuleRequested.connect(function (name) {
            console.log("[test] showRuleRequested fired with", name);
        });
        page.showRuleRequested("899-curl-allow");
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
            &QUrl::from("qrc:/inline_connections_page_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "ConnectionsPage QML probe failed: root object was null — a type/property error in \
         ConnectionsPage.qml (matched-rule inspector fields / showRuleRequested signal)"
    );

    let _ = app.as_mut();
}
