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

    // The event loop never ends if the probe fails to load or a step hangs
    // outside the QML watchdog: give up after 90 s, naming the stderr so far.
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog_done = finished.clone();
    std::thread::spawn(move || {
        for _ in 0..900 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if watchdog_done.load(Ordering::SeqCst) {
                return;
            }
        }
        let so_far = std::fs::read_to_string("/proc/self/fd/2").unwrap_or_default();
        let msg = format!("probe watchdog: no result after 90 s; stderr so far:\n{so_far}\n");
        // SAFETY: fd 1 is open for the whole process; the buffer is valid for its length.
        unsafe {
            libc::write(1, msg.as_ptr().cast(), msg.len());
        }
        std::process::exit(101);
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            let qml = qml.replace("    // @a11y-guard\n", GUARD).replace(
                "    // @list-keys\n",
                &KEYS.replace(
                    "@STEP_LIMIT@",
                    &std::env::var("SW_STEPS").unwrap_or_else(|_| "1000000".into()),
                ),
            );
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(url));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    finished.store(true, Ordering::SeqCst);
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
