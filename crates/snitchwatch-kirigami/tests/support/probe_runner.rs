//! Runs one QML probe window to completion and fails on any error it logs
//! (tests only). Included with `#[path]` beside the `accessibility` and
//! `key_repeat` test modules, which it switches on.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

use crate::common::{capture_stderr, init_headless_qt_env};

/// The shared helpers spliced into every list-page probe.
const GUARD: &str = include_str!("a11y_guard.qmlpart");
const KEYS: &str = include_str!("list_keys.qmlpart");

/// Load `qml` (its `// @a11y-guard` and `// @list-keys` lines replaced by the
/// shared helpers), run it until it quits, and panic unless it logged
/// `done_marker` and nothing else from `url`.
pub fn run(qml: &str, url: &str, done_marker: &str) {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    assert!(
        crate::accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
    assert!(crate::key_repeat::ffi::install_auto_repeat_keys());
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
            let qml = qml
                .replace("    // @a11y-guard\n", GUARD)
                .replace("    // @list-keys\n", KEYS);
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(url));
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
        .filter(|line| line.contains(url) || line.contains("TypeError"))
        .filter(|line| !line.contains(done_marker))
        .collect();
    assert!(bad.is_empty(), "probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(done_marker),
        "probe never finished; stderr:\n{captured}"
    );
}
