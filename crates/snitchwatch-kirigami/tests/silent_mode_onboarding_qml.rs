//! The onboarding step that asks how new connections are treated
//! (`SilentModeChoice.qml`) against a real `SettingsController` whose file
//! lives in a temporary `XDG_CONFIG_HOME`:
//!   * "Allow and record" is preselected while nothing is chosen;
//!   * choosing "Deny and record" shows the warning and offers the existing
//!     recommended-rules page (a signal main.qml answers), saving the choice;
//!   * Continue persists the choice and an unknown token is ignored;
//!   * the Allow duration defaults to Forever, the pre-rename `until_quit`
//!     token is stored as `until_restart`, and fields written by an older
//!     build (`rdap_enabled`) survive.
//!
//! Then the file is read back. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;
use snitchwatch_kirigami::settings::{read_settings, SilentMode};

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/silent_mode_onboarding_probe.qml";
const DONE_MARKER: &str = "SILENT_MODE_ONBOARDING_PROBE_DONE";

#[test]
fn onboarding_asks_for_the_silent_mode_and_saves_the_choice() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    let config = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", config.path());
    let settings_file = snitchwatch_kirigami::paths::settings_path();
    assert!(
        settings_file.starts_with(config.path()),
        "the probe must not touch the real settings: {settings_file:?}"
    );
    std::fs::create_dir_all(settings_file.parent().unwrap()).unwrap();
    std::fs::write(&settings_file, r#"{"rdap_enabled":true}"#).unwrap();

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
    width: 400
    height: 400
    property var failures: []
    property int phase: 0
    property real phaseStart: Date.now()
    property bool reviewAsked: false
    property bool chosenSignalled: false

    SettingsController { id: sc }
    SilentModeChoice {
        id: choice
        anchors.fill: parent
        settings: sc
        onReviewRecommendedRules: probe.reviewAsked = true
        onChosen: probe.chosenSignalled = true
    }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function elapsed() { return Date.now() - probe.phaseStart; }
    function next() { probe.phase++; probe.phaseStart = Date.now(); }

    Timer {
        interval: 20000
        running: true
        onTriggered: {
            try { throw new Error("onboarding probe timed out in phase " + probe.phase); }
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
                    probe.expect(sc.defaultDuration === "forever", "default duration " + sc.defaultDuration);
                    probe.expect(sc.silentMode === "" && sc.preferencesLoaded === false, "nothing chosen yet");
                    probe.expect(choice.selected === "allow", "Allow and record is preselected: " + choice.selected);
                    probe.expect(choice.denyWarningBox.visible === false, "no warning for Allow");
                    sc.refreshPreferences();
                    sc.chooseDefaultDuration("until_quit");
                    probe.next();
                } else if (probe.phase === 1 && probe.elapsed() > 400) {
                    probe.expect(sc.preferencesLoaded === true, "preferences loaded");
                    probe.expect(sc.defaultDuration === "until_restart",
                                 "the old token is renamed: " + sc.defaultDuration);
                    probe.expect(sc.silentMode === "", "still unchosen");
                    choice.selected = "deny";
                    probe.expect(choice.denyWarningBox.visible === true, "Deny shows the warning");
                    probe.expect(choice.denyWarningBox.text === SilentModes.denyWarning
                                 && SilentModes.denyWarning.indexOf("DNS") >= 0
                                 && SilentModes.denyWarning.indexOf("NetworkManager") >= 0,
                                 "warning names DNS and NetworkManager");
                    probe.expect(choice.denyWarningBox.actions.length === 1, "one curated-defaults offer");
                    choice.denyWarningBox.actions[0].trigger();
                    probe.expect(probe.reviewAsked === true, "the recommended-rules page is requested");
                    probe.next();
                } else if (probe.phase === 2 && probe.elapsed() > 400) {
                    probe.expect(sc.silentMode === "deny", "Deny saved: " + sc.silentMode);
                    sc.chooseSilentMode("bogus");
                    choice.selected = "allow";
                    probe.expect(choice.denyWarningBox.visible === false, "warning goes away");
                    choice.continueButton.clicked();
                    probe.expect(probe.chosenSignalled === true, "Continue reports the choice");
                    probe.next();
                } else if (probe.phase === 3 && probe.elapsed() > 400) {
                    probe.expect(sc.silentMode === "allow", "Allow saved over Deny: " + sc.silentMode);
                    done = true;
                }
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("onboarding probe: " + probe.failures.join("; "));
                    }
                    console.warn("SILENT_MODE_ONBOARDING_PROBE_DONE");
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
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
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

    assert!(
        root_ok.load(Ordering::SeqCst),
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL) || l.contains("qml/SilentModeChoice.qml"))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "onboarding probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );

    // What reached the disk: both choices, and the older field untouched.
    let saved = read_settings(&settings_file);
    assert_eq!(saved.silent_mode, Some(SilentMode::Allow));
    assert_eq!(saved.duration_token(), "until_restart");
    assert!(saved.rdap_enabled, "a field from an older build was lost");
}
