//! Headless QML-load smoke test for the Kirigami shell scaffold (Task 5).
//!
//! This is a cargo *integration test* (its own compilation unit), so it links
//! against the `snitchwatch_kirigami` library and, transitively, the cxx-qt
//! generated QML-module registration. The Phase 3a spike proved cxx-qt issue
//! #770 does not bite this repo's `tests/*` convention
//! (`crates/kirigami-spike/tests/link_770.rs`); this test relies on that.
//!
//! Load-bearing assertion: `objectCreated` fires with a non-null root object,
//! which means `main.qml` parsed, `org.kde.kirigami` resolved, and the Rust
//! `AppInfo` QML type registered. Because `main.qml` references the
//! `ConnectionsPage` type and instantiates a `ConnectionsModel` inline in its
//! object tree, this also transitively proves `ConnectionsPage.qml` compiles
//! and the model binds (a component-time type error there nulls the root —
//! verified). Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

// Force the cxx-qt generated bridge (and its static QML-type registration) to
// link into this test binary.
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[test]
fn main_qml_loads_with_kirigami_and_appinfo() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let fired = Arc::new(AtomicBool::new(false));
    let root_ok = Arc::new(AtomicBool::new(false));

    // Hold the connection guard across `load()` so the slot stays connected.
    // `QQmlApplicationEngine::load` is synchronous for local (qrc) URLs, so
    // `objectCreated` fires before `load` returns.
    let guard = engine.as_mut().map(|engine| {
        let fired = fired.clone();
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            fired.store(true, Ordering::SeqCst);
            // SAFETY: the pointer is only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml",
        ));
    }
    drop(guard);

    assert!(
        fired.load(Ordering::SeqCst),
        "objectCreated never fired: main.qml did not load at all"
    );
    assert!(
        root_ok.load(Ordering::SeqCst),
        "main.qml root object was null: Kirigami or the AppInfo QML type failed to resolve"
    );

    // Keep `app` alive until here (Qt requires a QGuiApplication for the engine).
    let _ = app.as_mut();
}
