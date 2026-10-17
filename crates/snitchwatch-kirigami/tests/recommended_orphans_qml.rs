//! #113 in the window: the real `RecommendedRulesPage.qml` with a real
//! `CuratedDefaultsModel`, and its "No longer recommended" section.
//!   * The section shows only when the bridge lists such rules (an older
//!     bridge never does).
//!   * Each row names the rule as plain text and says where it stands in
//!     fixed words, with the bridge's problem text.
//!   * Remove asks for a confirmation before anything is sent, and is not
//!     offered for a rule whose removal was refused (its file left) or on a
//!     bridge that changes no recommended rules.
//!
//! Fails on any QML warning from the probe or the shell's QML. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::curated_defaults_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/recommended_orphans_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn no_longer_recommended_rules_are_removed_only_once_confirmed() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 900
    height: 700

    readonly property string markup: "<b>refused</b>"
    property var sent: []

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probeWindow.find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    function send(orphans, unavailable) {
        const msg = { action: "setCuratedDefaults",
                      entries: [{ id: "flatpak", program: "/usr/bin/flatpak", allows: "a",
                                  why: "w", on: false, status: "off" }],
                      storage: { persistent: true } };
        if (orphans) msg.orphans = orphans;
        if (unavailable) msg.unavailable = unavailable;
        curated.applyServerMessageJson(JSON.stringify(msg));
    }

    function section() {
        const footer = page.entriesList.footerItem;
        probeWindow.expect(footer !== null, "no footer");
        return footer;
    }

    function orphanRow(index) {
        const rows = find(section(), "orphanRows");
        probeWindow.expect(rows !== null, "no orphan rows");
        const item = rows.itemAt(index);
        probeWindow.expect(item !== null, "no orphan row " + index);
        return item;
    }

    CuratedDefaultsModel { id: curated }

    Connections {
        target: curated
        function onCuratedChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    RecommendedRulesPage {
        id: page
        anchors.fill: parent
        model: curated
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                // An older bridge (or none to list): no section, nothing to ask.
                send(null);
                expect(!section().visible, "section shown with no orphans");
                curated.removeOrphan("old");
                expect(probeWindow.sent.length === 0, "asked for an unlisted rule");

                send([{ id: "gone", enabled: true, fileLeft: true, problem: "File stays." },
                      { id: "off", enabled: false, problem: probeWindow.markup },
                      { id: "old", enabled: true }]);
                expect(section().visible, "no section for listed orphans");
                expect(find(section(), "orphanRows").count === 3,
                       "rows: " + find(section(), "orphanRows").count);
                const name = find(orphanRow(2), "orphanName");
                expect(name.text === "snitchwatch-default-old" && name.textFormat === Text.PlainText,
                       "name: " + name.text);
                expect(find(orphanRow(2), "orphanStatus").text
                       === "In the firewall, but no longer in Snitchwatch's recommended rules.",
                       "status: " + find(orphanRow(2), "orphanStatus").text);
                expect(!find(orphanRow(2), "orphanProblem").visible, "a problem with none");
                // A disabled orphan says so; the bridge's problem is plain text.
                expect(find(orphanRow(1), "orphanStatus").text.indexOf(
                           "turned off on the Rules page") >= 0, "disabled orphan not said");
                const problem = find(orphanRow(1), "orphanProblem");
                expect(problem.visible && problem.text === probeWindow.markup
                       && problem.textFormat === Text.PlainText, "problem not plain text");
                expect(find(orphanRow(1), "orphanRemoveButton").visible,
                       "no Remove on a disabled orphan");
                // Refused, file left: said, and no second Remove.
                expect(find(orphanRow(0), "orphanStatus").text.indexOf("saved file may remain") >= 0,
                       "file left not said");
                expect(find(orphanRow(0), "orphanProblem").text === "File stays.", "no reason");
                expect(!find(orphanRow(0), "orphanRemoveButton").visible, "Remove on a file left");
                curated.removeOrphan("gone");
                expect(probeWindow.sent.length === 0, "asked for a refused one");

                // Remove asks first; Cancel cancels; confirm sends exactly it.
                const remove = find(orphanRow(2), "orphanRemoveButton");
                expect(remove.visible, "no Remove");
                remove.clicked();
                expect(probeWindow.sent.length === 0, "removed before the user confirmed");
                expect(find(orphanRow(2), "orphanRemoveQuestion").visible, "no question");
                find(orphanRow(2), "orphanRemoveCancel").clicked();
                expect(probeWindow.sent.length === 0
                       && find(orphanRow(2), "orphanRemoveButton").visible, "Cancel didn't cancel");
                find(orphanRow(2), "orphanRemoveButton").clicked();
                find(orphanRow(2), "orphanRemoveConfirm").clicked();
                expect(probeWindow.sent.length === 1
                       && probeWindow.sent[0].action === "removeCuratedOrphan"
                       && probeWindow.sent[0].id === "old",
                       "Remove: " + JSON.stringify(probeWindow.sent));

                // A bridge that changes none: listed, never removable.
                send([{ id: "old", enabled: true }], "Needs the system service.");
                expect(section().visible, "section hidden while unavailable");
                expect(!find(orphanRow(0), "orphanRemoveButton").visible,
                       "Remove while unavailable");
                curated.removeOrphan("old");
                expect(probeWindow.sent.length === 1, "asked while unavailable");

                // The bridge's next list without them: the section goes.
                send(null);
                expect(!section().visible, "section lingers");
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
        "recommended orphans probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "recommended orphans probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
