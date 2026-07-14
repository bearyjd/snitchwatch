//! Integration smoke: `DaemonHealthModel` is usable as a QML element (Task 10).
//!
//! Scope of this test — mirrors `rules_model_qml.rs`'s scope note:
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> derive-properties path without aborting (a Rust panic
//!     inside the invokable would abort this test binary), and
//!   * the resulting `hasProperty` — sorry, `hasProblem` — QML property
//!     reflects a real `DiagnosticsReport` containing one `Failed` check,
//!     asserted from QML itself (a thrown JS error in `Component.onCompleted`
//!     does not null the root object, so this assert is best-effort visibility,
//!     not load-bearing — the load-bearing assertion is the root-object-not-null
//!     check below, same as `rules_model_qml.rs`).
//!
//! The derivation logic itself (`has_problem`/`status_summary`/
//! `troubleshooting_text`) is covered exhaustively and Qt-free by
//! `daemon_health_model.rs`'s own unit tests; this test only proves the QML
//! registration and invokable wiring actually work end-to-end. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::daemon_health_model as _;

#[test]
fn daemon_health_model_registers_and_applies_report_json() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive a real DiagnosticsReport event
    // through it. The JSON literal's `kind`/`status` values match the real
    // `#[serde(rename_all = "snake_case")]` shape of `CheckKind`/`CheckStatus`
    // confirmed in `ws_messages.rs`'s `diagnostics_report_round_trips` test:
    // `CheckKind::EbpfSupport` -> "ebpf_support", and
    // `CheckStatus::Failed { detail }` (tag = "status") -> `{"status":"failed","detail":...}`.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell
QtObject {
    property DaemonHealthModel model: DaemonHealthModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "diagnosticsReport",
            checks: [
                { kind: "ebpf_support", status: { status: "failed", detail: "no BTF" } }
            ]
        }));
        if (!model.hasProblem) {
            throw new Error("expected hasProblem to be true after a failed check");
        }
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
            &QUrl::from("qrc:/inline_daemon_health_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "DaemonHealthModel QML probe failed: root object was null — the type failed to register \
         as a QML element or did not compile"
    );

    let _ = app.as_mut();
}
