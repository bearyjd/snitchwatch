//! Issue #51 follow-up, at runtime: the pages
//! `honest_ui_external_text_guards.rs` checks load and lay out with markup in
//! every piece of external text they show: daemon diagnostics, autostart and
//! coexistence details, the crash log, scanner errors and finding paths, and
//! the wizard detail. Fails on any QML warning from this probe or the shell's
//! own QML. `main.qml`'s bridge banner is checked by
//! `honest_ui_main_banner_qml.rs` (no event loop, like `smoke.rs`). Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/honest_ui_external_text_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn pages_showing_external_text_load_with_markup_in_it() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 900
    height: 700

    readonly property string markup: "<b>bold</b> <img src='https://example.invalid/x.png'> &amp;"
    readonly property string shell: "qrc:/qt/qml/com/snitchwatch/shell/qml/"
    property var made: []
    property string failure: ""

    SettingsController {
        id: settings
    }
    ScannerController {
        id: scanner
    }
    GeoModel {
        id: geo
    }
    WizardController {
        id: wizard
    }
    QtObject {
        id: health
        property bool hasProblem: true
        property string statusSummary: "Connection or kernel problem detected"
        property string troubleshootingText: probeWindow.markup
        function recheck() {}
    }

    // Every item named `name` under `item`.
    function findAll(item, name) {
        let found = [];
        if (!item) return found;
        if (item.objectName === name) found.push(item);
        for (let i = 0; i < item.children.length; i++) {
            found = found.concat(probeWindow.findAll(item.children[i], name));
        }
        return found;
    }

    // The labels that show external text, and that each page shows it as
    // plain text: a warning-free load alone would pass without `PlainText`.
    readonly property var externalTextLabels: [
        "daemonHealthTroubleshooting", "autostartError", "coexistenceDetail",
        "scannerErrorText", "scannerFindingPath", "wizardDetail", "geoDatabasePath"
    ]

    function checkPlainText() {
        probeWindow.setExternalText();
        for (const name of probeWindow.externalTextLabels) {
            let labels = [];
            for (const page of probeWindow.made) {
                labels = labels.concat(probeWindow.findAll(page, name));
            }
            if (labels.length === 0) {
                throw new Error("no label named " + name);
            }
            for (const label of labels) {
                if (label.textFormat !== Text.PlainText) {
                    throw new Error(name + " has textFormat " + label.textFormat);
                }
                if (label.text.indexOf(probeWindow.markup) < 0) {
                    throw new Error(name + " does not show the markup literally: " + label.text);
                }
            }
        }
    }

    // The coexistence instructions are dimmed as background text, but not
    // while there is a conflict: that is when the user needs them.
    function checkCoexistenceOpacity() {
        let labels = [];
        for (const page of probeWindow.made) {
            labels = labels.concat(probeWindow.findAll(page, "coexistenceDetail"));
        }
        const label = labels[0];
        if (label.opacity !== 1) {
            throw new Error("conflict: the instructions are dimmed to " + label.opacity);
        }
        settings.coexistenceConflict = false;
        if (label.opacity !== 0.7) {
            throw new Error("no conflict: the detail is at " + label.opacity);
        }
        settings.coexistenceConflict = true;
    }

    function make(page, parent, props) {
        const component = Qt.createComponent(probeWindow.shell + page);
        if (component.status !== Component.Ready) {
            throw new Error(page + ": " + component.errorString());
        }
        const object = component.createObject(parent, props);
        if (object === null) {
            throw new Error(page + " was not created");
        }
        probeWindow.made.push(object);
        return object;
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                probeWindow.populate();
            } catch (e) {
                probeWindow.failure = String(e);
            } finally {
                quitTimer.start();
            }
        }
    }
    // Pages refresh their controllers when they load and can clear what was
    // set before, so this runs again before the labels are checked.
    function setExternalText() {
            settings.autostartError = probeWindow.markup;
            settings.coexistenceConflict = true;
            settings.coexistenceDetail = probeWindow.markup;
            settings.crashLogText = probeWindow.markup;
            scanner.errorText = probeWindow.markup;
            scanner.reportJson = JSON.stringify({
                new: [{ path: probeWindow.markup, detail: probeWindow.markup }],
                still_outstanding: [], resolved: [], informational: [], skipped: []
            });
            wizard.detail = probeWindow.markup;
            geo.dbAvailable = false;
            geo.dbPath = probeWindow.markup;
    }
    function populate() {
        probeWindow.setExternalText();
            const area = probeWindow.contentItem;
            probeWindow.make("DaemonHealthPage.qml", area, { model: health });
            probeWindow.make("DiagnosticsPage.qml", area, { controller: settings });
            probeWindow.make("ScannerPage.qml", area, { controller: scanner });
            probeWindow.make("OnboardingPage.qml", area, { controller: wizard });
            probeWindow.make("GeoPage.qml", area, { model: geo });
    }
    // Lets the pages lay out (and any binding warning surface) first. A
    // `throw` here is reported against the probe URL; `finally` still quits.
    Timer {
        id: quitTimer
        interval: 300
        repeat: false
        onTriggered: {
            try {
                if (probeWindow.failure !== "") {
                    throw new Error("external-text probe: " + probeWindow.failure);
                }
                probeWindow.checkPlainText();
                probeWindow.checkCoexistenceOpacity();
            } finally {
                Qt.quit();
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
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "external-text probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML warnings while showing markup in external text:\n{}",
        bad_lines.join("\n")
    );
}
