//! Integration smoke: `PendingDecision` is usable as a QML element (Task 7).
//!
//! Proves the cxx-qt wrapper registers under `com.snitchwatch.shell` and that
//! its `submit` invokable + `verdictSubmitted` signal run the real
//! `build_verdict_message` -> serialize path without aborting (a Rust panic in
//! the invokable would abort this test binary; an unregistered type would null
//! the root). The verdict-mapping *correctness* is covered exhaustively and
//! Qt-free by the `pending_decision` unit tests. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::pending_decision as _;

#[test]
fn pending_decision_registers_and_submit_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the model, connect the verdict signal, and submit one real
    // decision through the build_verdict_message -> serialize path.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property string lastVerdict: ""
    property PendingDecision decision: PendingDecision {
        onVerdictSubmitted: (json) => root.lastVerdict = json
    }
    id: root
    Component.onCompleted: {
        decision.submit("r1", "deny_always", "any_host");
        console.log("[test] PendingDecision verdict json =", root.lastVerdict);
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
            &QUrl::from("qrc:/inline_pending_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "PendingDecision QML probe failed: root object was null — the type failed to \
         register as a QML element or the submit invokable panicked"
    );

    let _ = app.as_mut();
}
