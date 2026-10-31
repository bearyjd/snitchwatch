//! Selectable text, batch 7: on a real `PendingDecisionSheet` and
//! `MakeRuleSheet` (the latter with its form open), every converted
//! `SelectableText` is mouse only. It is not a Tab stop, a click on it leaves
//! the active-focus item where it was (and it never holds active focus), and
//! a Tab round visits none of them. Complements the source guards in
//! `selectable_batch7_guards.rs`. Run headless with
//! `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_batch7_focus_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_BATCH7_FOCUS_DONE";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900
    height: 1400
    readonly property string doneMarker: "SELECTABLE_BATCH7_FOCUS_DONE"
    // @a11y-guard
    // @list-keys
    property int ran: 0
    readonly property int expectedSteps: 4

    QtObject {
        id: feedStub
        function submitVerdict(rowId, choice, scope, duration, bindable) { return true; }
        function appBoundRulesFor(rowId) { return true; }
        function decideLater(rowId) { return true; }
        function decideLaterFor(rowId) { return true; }
    }
    QtObject {
        id: makeModel
        function makeRule(rowId, choice, scope, duration, requestId) { return ""; }
    }
    QtObject {
        id: makeController
        property string rowId: ""
        property string statusText: ""
        property bool busy: false
        function begin(rowId) { makeController.rowId = rowId; return "req-1"; }
        function notSent(why) { makeController.busy = false; makeController.statusText = why; }
    }

    ColumnLayout {
        width: parent.width
        Controls.Button { id: start; text: "Start here" }
        PendingDecisionSheet {
            id: decision
            Layout.fillWidth: true
            rowId: "1:asked"
            process: "app"
            host: "example.com"
            remoteIp: "93.184.216.34"
            bindableProcessPath: true
            appBoundRules: true
            bridgeFeed: feedStub
        }
        MakeRuleSheet {
            id: make
            Layout.fillWidth: true
            rowId: "1:putoff"
            model: makeModel
            controller: makeController
            bindableProcessPath: true
        }
    }

    function selectables(item, into) {
        if (!item) return into;
        if (item.selectByMouse !== undefined && item.contextMenu !== undefined) into.push(item);
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) probe.selectables(kids[i], into);
        return into;
    }
    function clickable(t) {
        return t.visible && t.width > 0 && t.height > 0 && t.text.length > 0;
    }
    function clickEach(root, label, atLeast) {
        const all = probe.selectables(root, []);
        let clicked = 0;
        for (const t of all) {
            probe.expect(t.activeFocusOnTab === false, label + ": '" + t.text + "' is a Tab stop");
            probe.expect(t.activeFocusOnPress === false, label + ": '" + t.text + "' takes focus on press");
            if (!probe.clickable(t)) continue;
            start.forceActiveFocus();
            const before = probe.activeFocusItem;
            probe.expect(before === start, label + ": setup focus is " + before);
            tc.mouseClick(t, 2, 2, Qt.LeftButton);
            probe.expect(!t.activeFocus, label + ": '" + t.text + "' has active focus after a click");
            probe.expect(probe.activeFocusItem === before,
                         label + ": a click on '" + t.text + "' moved focus to " + probe.activeFocusItem);
            ++clicked;
        }
        probe.expect(clicked >= atLeast, label + ": only " + clicked + " clickable texts (of " + all.length + ")");
        return all.length;
    }
    function tabRound() {
        start.forceActiveFocus();
        const visited = [];
        for (let i = 0; i < 40; ++i) {
            tc.keyClick(Qt.Key_Tab);
            const f = probe.activeFocusItem;
            if (f === start || !f) break;
            visited.push(f);
        }
        return visited;
    }

    Component.onCompleted: {
        probe.expect(probe.stepLimit >= probe.expectedSteps, "SW_STEPS cuts the probe short: " + probe.stepLimit);
        probe.step("decision sheet: clicks leave focus alone", function () {
            return decision.visible && decision.width > 0 ? "" : "the decision sheet";
        }, function () {
            probe.ran++;
            const n = probe.clickEach(decision, "decision", 4);
            probe.expect(n >= 10, "decision sheet: only " + n + " SelectableText found");
        });
        probe.step("open the Make a rule form", null, function () {
            probe.ran++;
            make.openButton.forceActiveFocus();
            tc.keyClick(Qt.Key_Space);
        });
        probe.step("make rule sheet: clicks leave focus alone; Tab skips every text", function () {
            return make.form.visible ? "" : "the form open";
        }, function () {
            probe.ran++;
            const n = probe.clickEach(make, "make", 2);
            probe.expect(n >= 5, "make rule sheet: only " + n + " SelectableText found");
            for (const f of probe.tabRound())
                probe.expect(!(f.selectByMouse !== undefined && f.contextMenu !== undefined),
                             "Tab reached a SelectableText: '" + f.text + "'");
        });
        probe.step("steps executed == " + probe.expectedSteps, null, function () {
            probe.expect(probe.ran + 1 === probe.expectedSteps,
                         "steps executed " + (probe.ran + 1) + " != " + probe.expectedSteps);
        });
    }
}
"##;

#[test]
fn batch_7_texts_never_take_focus() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
