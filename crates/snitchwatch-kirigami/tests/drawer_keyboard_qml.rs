//! Q01 slice 7: global navigation by keyboard alone, on the REAL `main.qml`
//! with REAL key events (QtTest) and Qt's accessibility layer on. From a
//! fresh window:
//!   * Tab reaches the drawer opener (a named button); Space opens the drawer
//!     and focus lands on its first entry; Tab stays inside the drawer;
//!   * the drawer lists exactly the 12 pages, in order, each named;
//!   * every entry fires on a fresh Return, Enter or Space, never on a held
//!     key (an auto-repeat, then its release);
//!   * after each switch focus is inside the NEW page (not the drawer, not
//!     the destroyed page);
//!   * Escape closes the drawer and focus returns to the opener; stray keys
//!     with the drawer closed switch nothing.
//!
//! Auto-repeats are real: `tests/support/key_repeat.h` re-sends a key given
//! `Qt.GroupSwitchModifier` flagged as an auto-repeat. Each phase waits on a
//! bounded readiness gate (#237) and a 45 s watchdog fails the probe naming
//! the stuck phase. Run headless with `QT_QPA_PLATFORM=offscreen`.
//!
//! Limit: this proves the key paths in Qt's offscreen platform. Orca
//! speech, focus rings and Plasma's window focus behaviour need a desktop.
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;

const PROBE_URL: &str = "qrc:/drawer_keyboard_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
const DONE_MARKER: &str = "DRAWER_KEYBOARD_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");
/// The probe body, spliced in at the `// @phases` line.
const PHASES: &str = include_str!("probes/drawer_keyboard_phases.qmlpart");

const QML: &str = include_str!("probes/drawer_keyboard_probe.qml");

#[test]
fn the_drawer_works_by_keyboard() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    // A fresh, empty home: the pages' first controls depend on what is stored
    // (learned programs, rules, settings), so the table below assumes none.
    let home = tempfile::tempdir().expect("temp home");
    for (var, dir) in [
        ("HOME", ""),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "run"),
    ] {
        let path = home.path().join(dir);
        std::fs::create_dir_all(&path).expect("create temp dir");
        std::env::set_var(var, &path);
    }

    let mut app = QGuiApplication::new();
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
    assert!(key_repeat::ffi::install_auto_repeat_keys());
    let mut engine = QQmlApplicationEngine::new();

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

    let bad: Vec<&str> = captured
        .lines()
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains(SHELL_QML_PREFIX)
                || line.contains("TypeError")
                || line.contains("drawer keyboard probe")
        })
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "drawer keyboard probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
