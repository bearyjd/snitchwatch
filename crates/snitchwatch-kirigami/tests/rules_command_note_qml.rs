//! Issue #152: the Rules page's own commands (the inspector's switch and
//! Delete) ask for a result, and the bridge's note reaches the user: inside
//! the inspector while it is open, below the page title once it closed.
//! Only an awaited result counts; a refusal puts the switch back even
//! though the bridge re-sends no list for it. Every note is plain text.
//!
//! Assertions are QML `throw`s, which Qt reports against the probe URL on
//! stderr; the Rust side fails on any such line.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_command_note_probe.qml";
const DONE_MARKER: &str = "RULES_COMMAND_NOTE_PROBE_DONE";

#[test]
fn the_rules_pages_own_commands_show_the_bridges_notes() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 800
    height: 600

    property var sent: []
    property int finished: 0

    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }
    function result(requestId, outcome) {
        rulesModel.applyServerMessageJson(JSON.stringify({
            action: "ruleCommandResult", requestId: requestId, outcome: outcome }));
    }
    function curl(enabled) {
        return { name: "899-curl", displayName: "899-curl", enabled: enabled,
                 action: "allow", duration: "always", description: "",
                 operator: { operand: "dest.host", data: "example.com" },
                 precedence: false, nolog: false, deletable: true };
    }
    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }
    function click(control) {
        control.toggle();
        control.toggled();
    }
    function last() {
        return probeWindow.sent[probeWindow.sent.length - 1];
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }
    Connections {
        target: rulesModel
        function onRuleChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
        function onCommandFinished() {
            probeWindow.finished++;
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const sw = page.inspectorEnabledSwitch;
                const inNote = page.inspectorCommandNote;
                const pageNote = page.rulesCommandNote;
                page.inspectorSheet.enter = null;
                page.inspectorSheet.exit = null;
                probeWindow.setRules([probeWindow.curl(true)]);
                expect(page.openRuleByName("899-curl"), "rule not found");

                // A refused turn-off: the daemon took it off; the note shows
                // where the user is, as plain text, and only for this id.
                probeWindow.click(sw);
                const off = probeWindow.last();
                expect(off.action === "updateRule" && off.rule.enabled === false,
                       "no turn-off sent: " + JSON.stringify(off));
                expect(typeof off.requestId === "string" && off.requestId.indexOf("rules-") === 0,
                       "the turn-off asked for no result: " + JSON.stringify(off));
                const note = "Turned off. <b>may come back on</b>";
                probeWindow.result("edit-1", { status: "okWithNote", note: "someone else's" });
                expect(rulesModel.commandNote === "", "another request's result was shown");
                // The bridge's list shows it off before the result.
                probeWindow.setRules([probeWindow.curl(false)]);
                probeWindow.result(off.requestId, { status: "okWithNote", note: note });
                expect(rulesModel.commandNote === note, "note: " + rulesModel.commandNote);
                expect(probeWindow.finished === 1, "finished " + probeWindow.finished);
                expect(inNote.visible && inNote.text === note && inNote.textFormat === Text.PlainText,
                       "inspector note: " + inNote.visible + " " + inNote.text);
                probeWindow.result(off.requestId, { status: "okWithNote", note: "again" });
                expect(rulesModel.commandNote === note && probeWindow.finished === 1,
                       "a second result for the same id counted");

                // A refusal re-sends no list: the switch still goes back to
                // what the list holds. Sending clears the last note.
                probeWindow.click(sw);
                const busy = probeWindow.last();
                expect(busy.rule.enabled === true, "no turn-on sent");
                expect(rulesModel.commandNote === "", "a new command kept the old note");
                expect(sw.checked === true, "switch did not move");
                probeWindow.result(busy.requestId, { status: "refused",
                    problems: [{ path: "name", reason: "busy" }] });
                expect(rulesModel.commandNote.indexOf("Not sent: ") === 0,
                       "refusal note: " + rulesModel.commandNote);
                expect(sw.checked === false && page.inspectEnabled === false,
                       "switch kept a state the list doesn't have");

                // A change the daemon may or may not have applied says so.
                probeWindow.click(sw);
                probeWindow.result(probeWindow.last().requestId,
                                   { status: "unsure", reason: "may or may not have applied" });
                expect(rulesModel.commandNote === "may or may not have applied",
                       "unsure note: " + rulesModel.commandNote);

                // Delete closes the inspector: the note shows below the title.
                page.confirmingDelete = true;
                rulesModel.deleteRule("899-curl");
                const del = probeWindow.last();
                expect(del.action === "deleteRule" && del.requestId.indexOf("rules-") === 0,
                       "delete asked for no result: " + JSON.stringify(del));
                page.inspectorSheet.close();
                probeWindow.result(del.requestId, { status: "okWithNote", note: "file may remain" });
                expect(pageNote.visible && pageNote.text === "file may remain"
                       && pageNote.textFormat === Text.PlainText,
                       "page note: " + pageNote.visible + " " + pageNote.text);

                // The inspector shows a note only on the rule it is about.
                const other = probeWindow.curl(true);
                other.name = "898-other";
                other.displayName = other.name;
                probeWindow.setRules([other]);
                expect(page.openRuleByName("898-other"), "other rule not found");
                expect(!inNote.visible, "another rule's note shown in the inspector");
                page.inspectorSheet.close();

                // An Ok clears it.
                probeWindow.setRules([probeWindow.curl(true)]);
                rulesModel.setEnabled("899-curl", false);
                probeWindow.result(probeWindow.last().requestId, { status: "ok" });
                expect(rulesModel.commandNote === "" && !pageNote.visible, "an Ok left a note");
                console.warn("RULES_COMMAND_NOTE_PROBE_DONE");
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
        // Without a root object nothing calls Qt.quit(): skip the loop and
        // let the assertion below report the load failure instead of hanging.
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RulesPage command note probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) && !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage command note probe reported errors:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
