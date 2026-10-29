//! Q01 slice 4: learning review by keyboard alone, with REAL key events
//! (QtTest) and every control named (the shared guard,
//! `tests/support/a11y_guard.qmlpart`, Qt's accessibility layer on):
//!   * the page: Tab goes through the start panel, the buttons, then each
//!     row's boxes in order; Space ticks a box; Return/Enter/Escape do not;
//!   * Review opens the summary; Tab reaches Apply only after every extra
//!     confirmation; Return/Enter never apply; Escape discards;
//!   * a key still held (auto-repeat, held across the dialog opening or the
//!     rows being replaced) never applies, ticks, stops or cancels;
//!   * the banner's Stop learning / Review and the batch's Cancel.
//!
//! Auto-repeats are real: `tests/support/key_repeat.h` re-sends a key given
//! `Qt.GroupSwitchModifier` flagged as an auto-repeat. Each phase waits on a
//! readiness gate (#237). Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;

const PROBE_URL: &str = "qrc:/learning_keyboard_probe.qml";
const DONE_MARKER: &str = "LEARNING_KEYBOARD_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");
/// The probe body, spliced in at the `// @phases` line.
const PHASES: &str = include_str!("probes/learning_keyboard_phases.qmlpart");

const QML: &str = include_str!("probes/learning_keyboard_probe.qml");

#[test]
fn the_learning_review_works_by_keyboard_and_names_every_control() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
    assert!(key_repeat::ffi::install_auto_repeat_keys());
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            let qml = QML
                .replace("    // @a11y-guard\n", GUARD)
                .replace("    // @phases\n", PHASES);
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "learning keyboard probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
