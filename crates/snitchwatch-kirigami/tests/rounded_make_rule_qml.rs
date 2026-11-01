//! Restyle of `MakeRuleSheet`: on a real sheet with its form open, every
//! swapped control (the "Make a rule..." button, the scope and duration
//! combo boxes, Allow and Deny) is at least `Theme.hitMin` high, and a Tab
//! round from outside visits exactly those four form controls (no new Tab
//! stop). Run headless with
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

const PROBE_URL: &str = "qrc:/rounded_make_rule_probe.qml";
const DONE_MARKER: &str = "ROUNDED_MAKE_RULE_DONE";

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
    height: 900
    readonly property string doneMarker: "ROUNDED_MAKE_RULE_DONE"
    // @a11y-guard
    // @list-keys
    property int ran: 0
    readonly property int expectedSteps: 3

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
        MakeRuleSheet {
            id: make
            Layout.fillWidth: true
            rowId: "1:putoff"
            model: makeModel
            controller: makeController
            bindableProcessPath: true
        }
    }

    function gather(item, pred, into) {
        if (!item) return into;
        if (pred(item)) into.push(item);
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) probe.gather(kids[i], pred, into);
        return into;
    }
    function byText(t) {
        const r = probe.gather(make, function (i) { return i.text === t && i.clicked !== undefined; }, []);
        probe.expect(r.length === 1, "expected one control with text '" + t + "', found " + r.length);
        return r[0];
    }
    function byName(n) {
        const r = probe.gather(make, function (i) { return i.objectName === n; }, []);
        probe.expect(r.length === 1, "expected one item named " + n + ", found " + r.length);
        return r[0];
    }

    Component.onCompleted: {
        probe.expect(probe.stepLimit >= probe.expectedSteps, "SW_STEPS cuts the probe short: " + probe.stepLimit);
        probe.step("open button is a hit-sized target", function () {
            return make.openButton.visible && make.openButton.width > 0 ? "" : "the open button";
        }, function () {
            probe.ran++;
            probe.expect(make.openButton.height >= Theme.hitMin,
                         "open button is " + make.openButton.height + " high");
            make.openButton.forceActiveFocus();
            tc.keyClick(Qt.Key_Space);
        });
        probe.step("form controls are hit-sized", function () {
            return make.form.visible ? "" : "the form open";
        }, function () {
            probe.ran++;
            const controls = [probe.byName("makeRuleScope"), probe.byName("makeRuleDuration"),
                              probe.byText("Allow"), probe.byText("Deny")];
            for (const c of controls)
                probe.expect(c.height >= Theme.hitMin, "'" + (c.text || c.objectName) + "' is " + c.height + " high");
            const visited = [];
            start.forceActiveFocus();
            for (let i = 0; i < 12; ++i) {
                tc.keyClick(Qt.Key_Tab);
                const f = probe.activeFocusItem;
                if (f === start || !f) break;
                if (visited.indexOf(f) < 0) visited.push(f);
            }
            probe.expect(visited.length === 4, "Tab visited " + visited.length + " items, expected 4");
            for (const c of controls)
                probe.expect(visited.indexOf(c) >= 0, "Tab never reached '" + (c.text || c.objectName) + "'");
        });
        probe.step("steps executed == " + probe.expectedSteps, null, function () {
            probe.expect(probe.ran + 1 === probe.expectedSteps,
                         "steps executed " + (probe.ran + 1) + " != " + probe.expectedSteps);
        });
    }
}
"##;

#[test]
fn make_rule_controls_are_hit_sized_with_no_new_tab_stops() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
