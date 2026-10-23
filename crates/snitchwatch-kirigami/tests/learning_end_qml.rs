//! Learning mode phase L1, end to end through the real `SettingsController`
//! property the alert host pushes into Rust: a learning window that ran out
//! while the GUI was closed must never leave `silentMode` at "allow", not even
//! when the restore cannot be written, and the first working write afterwards
//! puts the restored mode on disk and clears the notice.
//!
//! The settings file is a symlink (readable, but `write_settings` refuses it,
//! whatever the uid). A helper thread, armed when the QML root is created,
//! waits for the controller to report the failed restore and only then swaps in
//! a regular file; the probe retries an unrelated write until it works. Nothing
//! is sequenced by a fixed delay: only the overall deadline is a clock. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;
use snitchwatch_kirigami::settings::{
    read_settings, write_settings, Learning, Settings, SilentMode,
};

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/learning_end_probe.qml";
/// The longest the helper waits for the failed restore to be reported.
const SWAP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);
const DONE_MARKER: &str = "LEARNING_END_PROBE_DONE";

#[test]
fn an_expired_window_never_leaves_allow_applied_even_when_the_restore_fails() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    let config = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", config.path());
    let settings_file = snitchwatch_kirigami::paths::settings_path();
    assert!(settings_file.starts_with(config.path()));
    let dir = settings_file.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let real = dir.join("real.json");
    // A window that ended long ago, over a saved Allow; Deny came before it.
    let expired = Settings {
        silent_mode: Some(SilentMode::Allow),
        learning: Some(Learning::begin(1_000, 60_000, Some(SilentMode::Deny))),
        ..Settings::default()
    };
    write_settings(&real, &expired).unwrap();
    std::os::unix::fs::symlink(&real, &settings_file).unwrap();

    // Armed from the engine's object-created callback (the Qt thread, once the
    // root exists). It swaps the file after the failed restore is reported.
    let swapper = Arc::new(std::sync::Mutex::new(None));

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 200
    height: 200
    property var failures: []
    property int phase: 0
    property real phaseStart: Date.now()

    SettingsController { id: sc }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function elapsed() { return Date.now() - probe.phaseStart; }
    function next() { probe.phase++; probe.phaseStart = Date.now(); }

    Timer {
        interval: 20000
        running: true
        onTriggered: {
            try { throw new Error("learning probe timed out in phase " + probe.phase
                                  + " (loaded=" + sc.preferencesLoaded + ", mode=" + sc.silentMode
                                  + ", writeFailed=" + sc.settingsWriteFailed + ")"); }
            finally { Qt.quit(); }
        }
    }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    sc.refreshPreferences();
                    probe.next();
                } else if (probe.phase === 1 && sc.preferencesLoaded) {
                    // Polled until loaded; the overall deadline fails it clearly.
                    probe.expect(sc.silentMode === "deny", "restored, not allow: " + sc.silentMode);
                    probe.expect(sc.settingsWriteFailed === true, "the write failure is reported");
                    probe.next();
                } else if (probe.phase === 2) {
                    // An unrelated write, retried until the file is writable
                    // again (the helper swaps it once the failure is reported)
                    // and the notice clears. Every try before that must keep
                    // Deny applied.
                    if (sc.settingsWriteFailed === false && sc.defaultDuration === "until_restart") {
                        probe.expect(sc.silentMode === "deny", "still deny: " + sc.silentMode);
                        done = true;
                    } else {
                        probe.expect(sc.silentMode === "deny", "never allow while failing: " + sc.silentMode);
                        if (probe.elapsed() > 100) {
                            sc.chooseDefaultDuration("until_restart");
                            probe.phaseStart = Date.now();
                        }
                    }
                }
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("learning probe: " + probe.failures.join("; "));
                    }
                    console.warn("LEARNING_END_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        let swapper = swapper.clone();
        let (swap_path, swap_real) = (settings_file.clone(), real.clone());
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
            let (path, real) = (swap_path.clone(), swap_real.clone());
            let handle = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + SWAP_DEADLINE;
                while !snitchwatch_kirigami::settings_controller::settings_write_failed() {
                    if std::time::Instant::now() > deadline {
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                std::fs::remove_file(&path).unwrap();
                std::fs::copy(&real, &path).unwrap();
                true
            });
            *swapper.lock().unwrap() = Some(handle);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);
    let swapped = swapper.lock().unwrap().take().map(|h| h.join().unwrap());
    assert_eq!(
        swapped,
        Some(true),
        "the failed restore was never reported, so the file was never swapped:\n{captured}"
    );

    assert!(
        root_ok.load(Ordering::SeqCst),
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(bad.is_empty(), "learning probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );

    let saved = read_settings(&settings_file);
    assert_eq!(saved.silent_mode, Some(SilentMode::Deny));
    assert_eq!(saved.learning, None);
    assert_eq!(saved.duration_token(), "until_restart");
}
