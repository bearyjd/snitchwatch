//! Prompt-slot D in the window: the real `RecommendedRulesPage.qml` with a
//! real `CuratedDefaultsModel`.
//!   * Nothing is offered until the bridge sends the list; an older bridge
//!     never does, and the page says so.
//!   * Every entry is off unless the bridge says on; a switch click only
//!     asks, and the switch keeps the bridge's value until it answers.
//!   * Each row shows exactly what its rule allows, as plain text, and its
//!     status in fixed words.
//!   * "Turn all on/off" asks only for the entries not already that way.
//!   * Remove is offered only for a rule edited outside Snitchwatch, and
//!     asks for a confirmation before anything is sent.
//!   * On a bridge that never adds them, nothing can be turned on, and the
//!     reason is shown.
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

const PROBE_URL: &str = "qrc:/recommended_rules_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_page_offers_nothing_unasked_and_says_what_each_rule_allows() {
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

    readonly property string markup: "<b>/usr/bin/flatpak</b> may connect to <i>dl.flathub.org</i>"
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

    function entry(id, on, status) {
        return { id: id, program: "/usr/bin/" + id, allows: probeWindow.markup,
                 why: "Why " + id, on: on, status: status };
    }

    function send(entries, unavailable) {
        const msg = { action: "setCuratedDefaults", entries: entries,
                      storage: { persistent: true } };
        if (unavailable) msg.unavailable = unavailable;
        curated.applyServerMessageJson(JSON.stringify(msg));
    }

    function row(index) {
        const item = page.entriesList.itemAtIndex(index);
        probeWindow.expect(item !== null, "no delegate for row " + index);
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
                const header = page.header;
                // An older bridge: nothing offered, and the page says so.
                expect(find(header, "notOfferedBanner").visible, "no not-offered banner");
                expect(!find(header, "allOnButton").visible, "Turn all on before any list");
                curated.setEntry("flatpak", true);
                curated.setAll(true);
                expect(probeWindow.sent.length === 0, "asked before the bridge offered any");

                send([entry("flatpak", false, "off"), entry("chronyc", true, "installed"),
                      entry("nm", true, "editedByYou")]);
                expect(!find(header, "notOfferedBanner").visible, "not-offered banner stayed");
                expect(page.entriesList.count === 3, "rows: " + page.entriesList.count);
                const sw = find(row(0), "entrySwitch");
                expect(sw.enabled && !sw.checked, "an entry the bridge says is off shows on");
                expect(find(row(1), "entrySwitch").checked, "an entry turned on shows off");
                const allows = find(row(0), "allowsLabel");
                expect(allows.text === probeWindow.markup && allows.textFormat === Text.PlainText,
                       "what the rule allows isn't shown as plain text");
                expect(find(row(1), "statusLabel").text === "Rule installed.",
                       "status: " + find(row(1), "statusLabel").text);

                // A click only asks; the switch keeps the bridge's value.
                sw.toggle();
                sw.toggled();
                expect(probeWindow.sent.length === 1
                       && JSON.stringify(probeWindow.sent[0].ids) === '["flatpak"]'
                       && probeWindow.sent[0].on === true
                       && probeWindow.sent[0].action === "setCuratedDefaults",
                       "click not sent: " + JSON.stringify(probeWindow.sent));
                expect(!sw.checked, "the switch claims on before the bridge says so");

                find(header, "allOffButton").clicked();
                expect(probeWindow.sent.length === 2
                       && JSON.stringify(probeWindow.sent[1].ids) === '["chronyc","nm"]'
                       && probeWindow.sent[1].on === false,
                       "Turn all off: " + JSON.stringify(probeWindow.sent));

                // Keep: only for a rule already in the firewall, not chosen yet.
                expect(!find(row(1), "keepButton").visible, "Keep on a chosen rule");
                send([entry("flatpak", false, "off"), entry("chronyc", true, "installed"),
                      entry("nm", true, "editedByYou"), entry("old", true, "inFirewall")]);
                const keep = find(row(3), "keepButton");
                expect(keep.visible, "no Keep on an undecided rule");
                keep.clicked();
                expect(probeWindow.sent.length === 3
                       && JSON.stringify(probeWindow.sent[2].ids) === '["old"]'
                       && probeWindow.sent[2].on === true,
                       "Keep: " + JSON.stringify(probeWindow.sent));
                probeWindow.sent.pop();

                // Remove: only for the edited rule, and only once confirmed.
                expect(!find(row(1), "removeButton").visible, "Remove on an unedited rule");
                const remove = find(row(2), "removeButton");
                expect(remove.visible, "no Remove on an edited rule");
                remove.clicked();
                expect(probeWindow.sent.length === 2, "removed before the user confirmed");
                expect(find(row(2), "removeQuestion").visible, "no confirmation question");
                find(row(2), "removeCancel").clicked();
                expect(probeWindow.sent.length === 2 && find(row(2), "removeButton").visible,
                       "Cancel didn't cancel");
                find(row(2), "removeButton").clicked();
                find(row(2), "removeConfirm").clicked();
                expect(probeWindow.sent.length === 3
                       && probeWindow.sent[2].action === "removeCuratedDefault"
                       && probeWindow.sent[2].id === "nm",
                       "Remove: " + JSON.stringify(probeWindow.sent));

                // A bridge that never adds them: nothing can be turned on.
                send([entry("flatpak", false, "unavailable")], "Needs the system service.");
                expect(find(header, "unavailableBanner").visible, "no unavailable banner");
                expect(find(header, "unavailableReason").text === "Needs the system service.",
                       "reason not shown");
                expect(!find(header, "allOnButton").visible, "Turn all on while unavailable");
                expect(!find(row(0), "entrySwitch").enabled, "switch enabled while unavailable");
                curated.setEntry("flatpak", true);
                curated.removeEntry("flatpak");
                expect(probeWindow.sent.length === 3, "asked while unavailable");
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
        "recommended rules probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "recommended rules probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
