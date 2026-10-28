//! Q01 slice 3: the Rules page and its sheets by keyboard alone, with REAL
//! key events (QtTest `keyClick`), and every control named (the shared
//! runtime guard, `tests/support/a11y_guard.qmlpart`, with Qt's
//! accessibility layer on):
//!   * Tab reaches a rule row; Space opens its inspector; Escape closes it
//!     and gives focus back to the row;
//!   * the rule editor and the simulator: Tab goes through every field in
//!     visual order (top to bottom, left to right), and Escape closes the
//!     sheet without saving;
//!   * the guard runs with the page, the inspector, the editor, the
//!     simulator and the import preview open.
//!
//! Each phase waits on a readiness gate (#237). Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;

const PROBE_URL: &str = "qrc:/rules_keyboard_probe.qml";
const DONE_MARKER: &str = "RULES_KEYBOARD_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1200
    height: 1000
    property var failures: []
    property int phase: 0
    property int stalls: 0
    readonly property int maxStalls: 200
    property var focusBefore: null
    // @a11y-guard

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }
    TestCase { id: tc; name: "rules"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function focused() { return probe.activeFocusItem; }
    function text(item) { return item && item.text !== undefined ? String(item.text) : ""; }
    function rule(name, operator) {
        return { name: name, displayName: name, enabled: true, action: "deny",
                 duration: "always", description: "", operator: operator,
                 precedence: false, nolog: false };
    }
    function inside(item, ancestor) {
        for (let it = item; it; it = it.parent) if (it === ancestor) return true;
        return false;
    }
    function rowOf(item) {
        for (let it = item; it; it = it.parent) if (it.name !== undefined && it.displayName !== undefined) return it;
        return null;
    }
    function label(item) {
        return probe.text(item) || (item ? item.objectName || String(item.Accessible.name || "") : "") || String(item);
    }
    // Tab inside an open sheet until focus leaves it or comes back round,
    // recording each control's position: the order must be visual.
    function tabThrough(sheet, what) {
        const seen = [];
        let last = null;
        for (let i = 0; i < 60; ++i) {
            tc.keyClick(Qt.Key_Tab);
            const f = probe.focused();
            if (!probe.inside(f, sheet.contentItem) || seen.some(s => s.item === f)) break;
            const p = f.mapToItem(sheet.contentItem, 0, 0);
            seen.push({ item: f, x: p.x, y: p.y, label: probe.label(f) });
        }
        probe.expect(seen.length >= 3, what + ": Tab reached only " + seen.length + " controls");
        for (let i = 1; i < seen.length; ++i) {
            const a = seen[i - 1], b = seen[i];
            const ordered = b.y > a.y + 2 || (Math.abs(b.y - a.y) <= 2 && b.x >= a.x);
            if (!ordered) probe.failures.push(what + ": Tab goes from '" + a.label + "' (" + Math.round(a.x) + ","
                + Math.round(a.y) + ") back to '" + b.label + "' (" + Math.round(b.x) + "," + Math.round(b.y) + ")");
        }
        return seen;
    }
    function rowItem(i) { return page.rulesList.itemAtIndex(i); }
    function button(root, label) {
        const b = probe.find(root, it => it.clicked !== undefined && probe.text(it) === label && it.visible);
        probe.expect(b !== null, "no button " + label);
        return b;
    }
    // Exact names, and "<label>: <value>" for each labelled selector.
    function expectNames(where, exact, labelled) {
        for (const n of exact) probe.expect(probe.names[n] === true, where + ": no control named '" + n + "'");
        for (const l of labelled)
            probe.expect(Object.keys(probe.names).some(n => n.indexOf(l + ": ") === 0 && n.length > l.length + 2),
                         where + ": no selector named '" + l + ": <value>'");
    }

    function gate(phase) {
        switch (phase) {
        case 1: return page.rulesList.count === 2 && probe.stable("r1", probe.rowItem(1)) ? "" : "two rule rows";
        case 2: return page.inspectorSheet.opened ? "" : "the inspector open";
        case 3: return !page.inspectorSheet.visible ? "" : "the inspector closed";
        case 4: case 5: return page.ruleEditor.opened ? "" : "the editor open";
        case 6: return !page.ruleEditor.visible ? "" : "the editor closed";
        case 7: return page.simulatorSheet.opened ? "" : "the simulator open";
        case 8: return !page.simulatorSheet.visible ? "" : "the simulator closed";
        case 9: return page.importSheet.opened ? "" : "the import preview open";
        case 10: return !page.importSheet.visible ? "" : "the import preview closed";
        case 11: return page.inspectorSheet.opened ? "" : "the inspector open by Return";
        default: return "";
        }
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const waiting = probe.gate(probe.phase);
                if (waiting !== "") {
                    if (++probe.stalls < probe.maxStalls) return;
                    throw new Error("phase " + probe.phase + " never ready for " + waiting);
                }
                probe.stalls = 0;
                if (probe.phase === 0) {
                    probe.requestActivate();
                    for (const sheet of [page.inspectorSheet, page.ruleEditor, page.simulatorSheet, page.importSheet]) {
                        sheet.enter = null;
                        sheet.exit = null;
                    }
                    rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: [
                        probe.rule("899-curl", { type: "simple", operand: "process.path", data: "/usr/bin/curl" }),
                        probe.rule("899-wget", { type: "simple", operand: "process.path", data: "/usr/bin/wget" })] }));
                } else if (probe.phase === 1) {
                    probe.sweep("rules page");
                    // Tab to a rule row, Space opens its inspector.
                    let presses = 0;
                    for (; presses < 40 && !page.rulesList.activeFocus; ++presses) tc.keyClick(Qt.Key_Tab);
                    probe.expect(presses > 0 && page.rulesList.activeFocus, "Tab never reaches the rules list");
                    probe.expect(page.rulesList.currentIndex === 0, "focusing the list selected no rule");
                    tc.keyClick(Qt.Key_Down);
                    probe.expect(page.rulesList.currentIndex === 1, "Down did not move to the next rule");
                    probe.focusBefore = probe.focused();
                    tc.keyClick(Qt.Key_Space);
                } else if (probe.phase === 2) {
                    probe.expect(page.inspectorSheet.ruleName === "899-wget" || String(page.inspectorSheet.title).indexOf("899-wget") >= 0,
                                 "Space opened the wrong rule: " + page.inspectorSheet.title);
                    probe.sweep("inspector");
                    probe.expectNames("inspector", ["Enabled", "Edit…", "Delete rule"], []);
                    tc.keyClick(Qt.Key_Escape);
                } else if (probe.phase === 3) {
                    probe.expect(probe.focused() === probe.focusBefore,
                                 "Escape did not give focus back to the rule row: " + probe.label(probe.focused()));
                    probe.expect(page.openEditor(""), "New rule did not open");
                } else if (probe.phase === 4) {
                    probe.sweep("editor");
                    probe.expectNames("editor", ["Name", "Description", "On", "Suggest", "Add condition"], ["Action", "Lasts"]);
                    probe.tabThrough(page.ruleEditor, "editor");
                    probe.button(page.ruleEditor.contentItem, "Show advanced options").clicked();
                    probe.sweep("editor, advanced");
                    probe.expectNames("editor, advanced", ["Decide first", "Don't log"], []);
                } else if (probe.phase === 5) {
                    const before = page.rulesList.count;
                    tc.keyClick(Qt.Key_Escape);
                    probe.expect(page.rulesList.count === before, "Escape saved a rule");
                } else if (probe.phase === 6) {
                    page.openSimulator("{}");
                } else if (probe.phase === 7) {
                    probe.sweep("simulator");
                    probe.expectNames("simulator", ["Process path", "Destination host", "Destination port"], ["Protocol"]);
                    probe.tabThrough(page.simulatorSheet, "simulator");
                    probe.button(page.simulatorSheet.contentItem, "Advanced inputs").clicked();
                    probe.sweep("simulator, advanced");
                    probe.expectNames("simulator, advanced", ["User ID", "Environment", "Command line", "Destination IP"], ["Checksums"]);
                    tc.keyClick(Qt.Key_Escape);
                } else if (probe.phase === 8) {
                    page.importSheet.open();
                } else if (probe.phase === 9) {
                    probe.sweep("import preview");
                    page.importSheet.close();
                } else if (probe.phase === 10) {
                    // Return opens the selected rule too.
                    page.rulesList.forceActiveFocus();
                    tc.keyClick(Qt.Key_Up);
                    tc.keyClick(Qt.Key_Return);
                } else if (probe.phase === 11) {
                    probe.expect(String(page.inspectorSheet.title).indexOf("899-curl") >= 0,
                                 "Return opened the wrong rule: " + page.inspectorSheet.title);
                    page.inspectorSheet.close();
                    const bad = Object.keys(probe.missing);
                    probe.expect(bad.length === 0, bad.length + " controls lack a meaningful Accessible.name: "
                        + bad.map(k => k + " (" + probe.missing[k] + ")").join("; "));
                    done = true;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("rules probe: " + probe.failures.join("; "));
                    }
                    console.warn("RULES_KEYBOARD_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn the_rules_page_and_its_sheets_work_by_keyboard_and_name_every_control() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    // As an assistive technology would; Controls fill in their accessible
    // role and name only then.
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
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
            let qml = QML.replace("    // @a11y-guard\n", GUARD);
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
    assert!(bad.is_empty(), "rules probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
