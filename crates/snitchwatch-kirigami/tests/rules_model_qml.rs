//! Integration smoke: `RulesModel` is usable as a QML element (Task 10).
//!
//! Scope of this test — and, deliberately, only this (mirrors
//! `blocklists_model_qml.rs`'s scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile against its `QAbstractListModel` base), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> row-store-apply path without aborting (a Rust panic
//!     inside the invokable would abort this test binary), and
//!   * `toggleEnabled`/`deleteRule` run the real emit-signal path without
//!     aborting.
//!
//! It intentionally does NOT assert row counts/content: a thrown JS error in
//! `Component.onCompleted` does not null the root object, so QML-side asserts
//! are not load-bearing. The model's ordering/CRUD *correctness* is covered
//! exhaustively and Qt-free by the `rules::row_store` unit tests, which are
//! what Task 10's acceptance criterion calls for. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::rules_model as _;

#[test]
fn rules_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive real events through it: a SetRules
    // insert (one user rule, one blocklist-band rule), an UpdateRules upsert,
    // plus one toggleEnabled/deleteRule call each. If any invokable panicked,
    // the whole test binary would abort; if the type were unregistered, the
    // root would be null.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property RulesModel model: RulesModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "setRules",
            rules: [
                { name: "899-firefox-allow-out", enabled: true, action: "allow",
                  duration: "always", description: "",
                  operator: { operand: "process.path", data: "/usr/bin/firefox" } },
                { name: "z00-blocklist:stevenblack:0001-doubleclick.net", enabled: true,
                  action: "deny", duration: "always", description: "",
                  operator: { operand: "dest.host", data: "doubleclick.net" } }
            ]
        }));
        model.applyServerMessageJson(JSON.stringify({
            action: "updateRules",
            rules: [
                { name: "899-firefox-allow-out", enabled: false, action: "allow",
                  duration: "always", description: "",
                  operator: { operand: "process.path", data: "/usr/bin/firefox" } }
            ]
        }));
        model.toggleEnabled("899-firefox-allow-out");
        model.deleteRule("z00-blocklist:stevenblack:0001-doubleclick.net");
        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] RulesModel.count after insert =", model.count);
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
            &QUrl::from("qrc:/inline_rules_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RulesModel QML probe failed: root object was null — the type failed to register as a \
         QML element or did not compile against its QAbstractListModel base"
    );

    let _ = app.as_mut();
}
