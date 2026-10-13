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
//!     or removed (age-out).
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
        }
    ]

    Timer {
        interval: 120
        running: true
        repeat: true
        onTriggered: {
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
            try {
                probe.steps[probe.step]();
            } catch (e) {
                probe.failures.push("step " + probe.step + " threw: " + e);
            }
            probe.step++;
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
