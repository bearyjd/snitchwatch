//! Inspector side panel (monitor-tree step 5), wide layout. A real
//! `ConnectionsPage` hosted without a page stack shows the panel as the
//! right-hand column at every width. Covers:
//!
//!   * nothing selected: the fixed placeholder, not empty space;
//!   * a header row click toggles its group and selects nothing;
//!   * a REAL mouse click on a connection row shows that row's host;
//!   * the verdict actions call the same bridge feed method as before, the
//!     "Simulate this connection" and "Show rule" buttons emit what they did
//!     in the sheet, and finishing an action clears the selection;
//!   * the close button clears the selection;
//!   * the selection is cleared when the selected row is hidden by a filter
//!     or removed (age-out);
//!   * auto-select never replaces a panel that is showing a row;
//!   * the arrow keys move the selection and the panel follows;
//!   * after a model reset the list's current index is re-anchored to the
//!     selected row;
//!   * the decision sheet's insight values wrap inside the column.
//!
//! One probe, steps run one per timer tick so the list can lay out between
//! them. Failures are collected and thrown together. Headless:
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/inspector_panel_wide_probe.qml";
const DONE_MARKER: &str = "INSPECTOR_PANEL_WIDE_PROBE_DONE";

#[test]
fn the_panel_follows_the_selection_and_clears_when_the_row_goes() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1200
    height: 700
    property var failures: []
    property int step: 0
    // Timer ticks fired from inside a click's nested event loop must not re-enter a step.
    property bool inStep: false
    property var submitted: []
    property var simulated: []
    property var shownRules: []

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {
            probe.submitted.push(rowId + " " + choice);
            return true;
        }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
        onSimulateConnectionRequested: function (json) { probe.simulated.push(json); }
        onShowRuleRequested: function (name) { probe.shownRules.push(name); }
    }
    TestCase { id: tc; name: "panel"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host, action, rule) {
        return { id: id, process: "/usr/bin/curl", processPath: "/usr/bin/curl",
                 dstHost: host, dstIp: "203.0.113.7", dstPort: 443, protocol: "tcp",
                 direction: "outgoing", action: action, bytesSent: 0, bytesReceived: 0,
                 startedAtMs: 1, matchedRule: rule };
    }
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probe.find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }
    function findText(item, text) {
        if (!item) return null;
        if (item.text === text && typeof item.clicked === "function") return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probe.findText(item.children[i], text);
            if (hit) return hit;
        }
        return null;
    }
    function findText2(item, text) {
        if (!item) return null;
        if (item.text === text) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probe.findText2(item.children[i], text);
            if (hit) return hit;
        }
        return null;
    }
    function delegateFor(rowId) {
        const list = page.connectionList;
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === rowId) return d;
        }
        return null;
    }
    function select(rowId) {
        const d = probe.delegateFor(rowId);
        probe.expect(d !== null, "no delegate for " + rowId);
        if (d) tc.mouseClick(d, 2, 2);
    }
    function panelHost() {
        const label = probe.find(page, "inspectorHost");
        return label ? label.text : null;
    }
    function placeholderShown() {
        const p = probe.find(page, "inspectorPlaceholder");
        return p !== null && p.visible;
    }
    function cleared(what) {
        probe.expect(!page.inspecting, what + ": still inspecting " + page.inspectId);
        probe.expect(probe.placeholderShown(), what + ": placeholder not shown");
        probe.expect(page.connectionList.currentIndex === -1, what + ": currentIndex "
                     + page.connectionList.currentIndex);
    }

    readonly property var steps: [
        function () {
            probe.cleared("initially");
            probe.expect(!page.compact, "a page without a page stack must not be compact");
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("a", "a.example.com", "allow", null),
                probe.row("b", "b.example.com", "allow", "my-rule"),
                probe.row("p", "p.example.com", null, null)] }));
        },
        // Grouped (the default): a header click toggles, never selects.
        function () {
            const d = page.connectionList.itemAtIndex(0);
            probe.expect(d && d.isGroupHeader, "first grouped row is not a header");
            if (d) tc.mouseClick(d, 2, 2);
            probe.cleared("after a header click");
            model.setGroupedMode(false);
        },
        function () {
            probe.select("a");
            probe.expect(page.inspecting && page.inspectId === "a", "click did not select a");
            probe.expect(probe.panelHost() === "a.example.com", "panel host: " + probe.panelHost());
            probe.expect(!probe.placeholderShown(), "placeholder shown with a selection");
            probe.expect(probe.find(page, "inspectorTitle").text === "/usr/bin/curl", "panel title");
            // Close button.
            probe.find(page, "inspectorCloseButton").clicked();
            probe.cleared("after the close button");
        },
        function () {
            // Simulate this connection: same prefill the sheet sent.
            probe.select("a");
            probe.find(page, "simulateConnectionButton").clicked();
            probe.expect(probe.simulated.length === 1
                         && probe.simulated[0] === model.simulationPrefillJson("a"),
                         "simulate request: " + JSON.stringify(probe.simulated));
            probe.cleared("after Simulate");
        },
        function () {
            // Show rule on a rule-decided row.
            probe.select("b");
            probe.expect(probe.panelHost() === "b.example.com", "panel host b: " + probe.panelHost());
            const button = probe.findText(page, "Show rule");
            probe.expect(button !== null && button.visible, "no visible Show rule button");
            if (button) button.clicked();
            probe.expect(probe.shownRules.length === 1 && probe.shownRules[0] === "my-rule",
                         "show rule: " + JSON.stringify(probe.shownRules));
            probe.cleared("after Show rule");
        },
        function () {
            // A pending row: the decision sheet is offered and Allow reaches the feed.
            probe.select("p");
            probe.expect(page.decisionSheet.visible, "decision sheet hidden for a pending row");
            probe.expect(probe.findText(page, "Simulate this connection") !== null, "Simulate text");
            page.decisionSheet.submit("allow");
            probe.expect(probe.submitted.length === 1 && probe.submitted[0] === "p allow",
                         "verdict sent: " + JSON.stringify(probe.submitted));
            probe.cleared("after a verdict");
        },
        function () {
            // Filter hides the selected row.
            probe.select("a");
            probe.expect(page.inspecting, "a not selected before the filter");
            model.setFilterQuery("b.example");
            probe.cleared("after a filter hid the row");
            model.setFilterQuery("");
        },
        function () {
            // Age-out removes the selected row.
            probe.select("b");
            probe.expect(page.inspecting && page.inspectId === "b", "b not selected");
            model.applyServerMessageJson(JSON.stringify({ action: "removeConnectionRows", ids: ["b"] }));
            probe.cleared("after the row aged out");
        },
        function () {
            // An unrelated removal keeps the selection.
            probe.select("a");
            model.applyServerMessageJson(JSON.stringify({ action: "removeConnectionRows", ids: ["p"] }));
            probe.expect(page.inspecting && page.inspectId === "a", "unrelated removal cleared the panel");
            // Auto-select must not replace a panel the user is looking at.
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("p2", "p2.example.com", null, null)] }));
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "a",
                         "auto-select replaced the panel: " + page.inspectId);
            probe.expect(probe.panelHost() === "a.example.com", "panel host changed: " + probe.panelHost());
            // Idle (nothing selected), a new pending row IS shown.
            page.dismissInspector();
            probe.expect(page.inspectId === "", "dismiss kept the id: " + page.inspectId);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("p3", "p3.example.com", null, null)] }));
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "p2",
                         "dismissing did not show the oldest pending row: " + page.inspectId);
            // The decision sheet's insight form wraps inside the 22-unit column.
            const note = probe.findText2(page, "Online research disabled — enable in Settings");
            probe.expect(note !== null, "no Registration info note");
            if (note) {
                const right = note.mapToItem(page.activePanel, note.width, 0).x;
                probe.expect(note.wrapMode === Text.Wrap, "Registration info does not wrap");
                probe.expect(right <= page.activePanel.width + 1,
                             "Registration info overflows the panel: " + right + " > " + page.activePanel.width);
                probe.expect(!note.truncated, "Registration info is clipped");
            }
        },
        function () {
            // Keyboard: the list takes focus and the arrows move the selection,
            // which the panel follows.
            page.dismissInspector();
            const list = page.connectionList;
            probe.expect(list.activeFocusOnTab && list.keyNavigationEnabled, "list lost keyboard navigation");
            probe.select("a");
            list.forceActiveFocus();
            const before = list.currentIndex;
            tc.keyClick(Qt.Key_Down);
            probe.expect(list.currentIndex === before + 1, "Down did not move: " + before + " -> " + list.currentIndex);
            const d = list.itemAtIndex(list.currentIndex);
            probe.expect(d && page.inspectId === d.rowId, "panel did not follow the arrow key: " + page.inspectId);
        },
        function () {
            // After a reset the list's index is re-anchored to the selected row.
            probe.select("p2");
            probe.expect(page.inspectId === "p2", "p2 not selected");
            model.setFilterQuery("p2.example");
        },
        function () {
            const at = model.visibleRowIndex("p2");
            probe.expect(page.inspectId === "p2" && at === 0, "p2 lost or misplaced by the filter: " + at);
            probe.expect(page.connectionList.currentIndex === at,
                         "currentIndex not re-anchored: " + page.connectionList.currentIndex + " vs " + at);
            model.setFilterQuery("");
        },
        function () {
            const at = model.visibleRowIndex("p2");
            probe.expect(at > 0 && page.connectionList.currentIndex === at,
                         "currentIndex not re-anchored after the filter cleared: "
                         + page.connectionList.currentIndex + " vs " + at);
        },
        function () {
            // A pending row arriving while the panel shows another row is not
            // shown (and not adopted by the model); closing the panel shows it.
            model.applyServerMessageJson(JSON.stringify({ action: "removeConnectionRows",
                                                          ids: ["p2", "p3"] }));
            probe.select("a");
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("p9", "p9.example.com", null, null)] }));
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "a",
                         "a pending arrival replaced the panel: " + page.inspectId);
            page.dismissInspector();
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "p9",
                         "closing the panel did not select the pending row: '" + page.inspectId + "'");
            // Settled and idle: nothing selected, nothing left pending to pick.
            page.dismissInspector();
        },
        function () {
            probe.expect(!page.inspecting, "dismiss left a selection: " + page.inspectId);
            // p9 was dismissed and is still pending (and older): the new
            // arrival is shown, not p9.
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                Object.assign(probe.row("p10", "p10.example.com", null, null), { startedAtMs: 2 })] }));
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "p10",
                         "idle: a newly arrived pending row was not shown (dismissed p9 must not return): '" + page.inspectId + "'");
            // Issue #146: every dismissed row is remembered, not just the
            // last. Dismiss p10 too; the next arrival is shown, not p9.
            page.dismissInspector();
        },
        function () {
            probe.expect(!page.inspecting, "dismissing p10 selected a dismissed row: " + page.inspectId);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                Object.assign(probe.row("p11", "p11.example.com", null, null), { startedAtMs: 3 })] }));
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "p11",
                         "a dismissed row (p9 or p10) came back instead of p11: '" + page.inspectId + "'");
            // Once p9 is decided it leaves the set; a dismissed row that is
            // decided and then asked again would not stay hidden.
            model.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [
                probe.row("p9", "p9.example.com", "allow", "r")] }));
            page.dismissInspector();
        },
        function () {
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                Object.assign(probe.row("p12", "p12.example.com", null, null), { startedAtMs: 4 })] }));
        },
        function () {
            probe.expect(page.dismissedIds.indexOf("p9") < 0,
                         "a decided row stayed in the dismissed set: " + JSON.stringify(page.dismissedIds));
            probe.expect(page.dismissedIds.indexOf("p10") >= 0 && page.dismissedIds.indexOf("p11") >= 0,
                         "still-pending dismissed rows were pruned: " + JSON.stringify(page.dismissedIds));
            probe.expect(page.inspecting && page.inspectId === "p12",
                         "p12 not shown: '" + page.inspectId + "'");
        }
    ]

    Timer {
        interval: 120
        running: true
        repeat: true
        onTriggered: {
            if (probe.inStep) return;
            if (probe.step >= probe.steps.length) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("inspector panel probe: " + probe.failures.join("; "));
                    }
                    console.warn("INSPECTOR_PANEL_WIDE_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
                return;
            }
            probe.inStep = true;
            try {
                probe.steps[probe.step]();
            } catch (e) {
                probe.failures.push("step " + probe.step + " threw: " + e);
            } finally {
                probe.step++;
                probe.inStep = false;
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
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "inspector panel (wide) probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
