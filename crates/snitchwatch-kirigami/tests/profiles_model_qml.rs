//! Integration smoke: `ProfilesModel` is usable as a QML element.
//!
//! Scope of this test — and, deliberately, only this (mirrors
//! `rules_model_qml.rs`'s scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile against its `QAbstractListModel` base), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> row-store-apply path without aborting (a Rust panic
//!     inside the invokable would abort this test binary), and
//!   * `createProfile`/`renameProfile`/`updateMatchers`/`activateProfile`/
//!     `deactivateProfile`/`deleteProfile` run the real emit-signal path
//!     without aborting.
//!
//! It intentionally does NOT assert row counts/content: a thrown JS error in
//! `Component.onCompleted` does not null the root object, so QML-side asserts
//! are not load-bearing. The model's ordering/CRUD *correctness* is covered
//! exhaustively and Qt-free by the `profiles::row_store` unit tests. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::profiles_model as _;

#[test]
fn profiles_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive real events through it: a
    // SetProfiles insert (two profiles, one active), a ProfileChanged
    // switch, plus one call each to every mutating invokable. If any
    // invokable panicked, the whole test binary would abort; if the type
    // were unregistered, the root would be null.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property ProfilesModel model: ProfilesModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "setProfiles",
            profiles: [
                { id: "home", name: "At Home", networkMatchers: ["Home*"],
                  rules: [], active: true },
                { id: "office", name: "Office", networkMatchers: ["Office*"],
                  rules: [], active: false }
            ]
        }));
        model.applyServerMessageJson(JSON.stringify({
            action: "profileChanged",
            activeProfileId: "office"
        }));
        model.createProfile("Public Wi-Fi", "Coffee*, Airport*");
        model.renameProfile("home", "At Home (renamed)");
        model.updateMatchers("home", "Home*, Home-Guest");
        model.activateProfile("home");
        model.deactivateProfile();
        model.deleteProfile("office");
        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] ProfilesModel.count after insert =", model.count);
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
            &QUrl::from("qrc:/inline_profiles_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "ProfilesModel QML probe failed: root object was null — the type failed to register as \
         a QML element or did not compile against its QAbstractListModel base"
    );

    let _ = app.as_mut();
}
