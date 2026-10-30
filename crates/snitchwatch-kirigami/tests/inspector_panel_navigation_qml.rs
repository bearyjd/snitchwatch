//! Navigation away from the Connections page while its detail page is pushed
//! (narrow window). PageRow.replace swaps the TOPMOST page, which is the detail
//! page, so without `StackNav` the new page replaced the wrong one or
//! the Connections page stayed stacked underneath. Wired here the way main.qml
//! wires it (the router goes through `StackNav.replaceMain`):
//!
//!   * "Show rule" from the detail page leaves the new page as the only page;
//!   * "Simulate this connection" likewise;
//!   * drawer-style navigation with the detail page open likewise.
//!
//! One probe, one step per timer tick (a pop is animated). Headless:
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/inspector_panel_navigation_probe.qml";
const DONE_MARKER: &str = "INSPECTOR_PANEL_NAVIGATION_PROBE_DONE";

#[test]
fn navigating_away_with_the_detail_page_open_leaves_one_page() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ApplicationWindow {
    id: probe
    visible: true
    width: 600
    height: 700
    property var failures: []
    property int step: 0
    property var simulated: []
    // tc.mouseClick runs the event loop; under load the next tick can fire
    // inside a step and re-run it before `step` advances.
    property bool inStep: false
    property int stalls: 0

    ConnectionsModel { id: connModel }
    StackNav { id: nav; stack: probe.pageStack }
    Component {
        id: rulesStub
        Kirigami.Page { objectName: "rulesStub" }
    }
    Component {
        id: connectionsComponent
        ConnectionsPage {
            objectName: "connectionsPage"
            model: connModel
            onShowRuleRequested: function (name) { nav.replaceMain(rulesStub); }
            onSimulateConnectionRequested: function (json) {
                probe.simulated.push(json);
                nav.replaceMain(rulesStub);
            }
        }
    }
    pageStack.initialPage: nav.createPage(connectionsComponent)
    TestCase { id: tc; name: "nav"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
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
    // A freshly replaced Connections page is not compact (so a row click
    // pushes no detail page) until it is attached to the window and sized, and
    // its row delegate is 0 wide and still moving until the list has laid out
    // (a click at (2, 2) then misses it). Ready = the delegate has a width and
    // sat at the same place on two consecutive ticks.
    property string lastRowPos: ""
    function connectionsReady() {
        if (probe.pageStack.depth !== 1) return false;
        const page = probe.pageStack.get(0);
        if (!page || page.objectName !== "connectionsPage" || !page.compact) return false;
        const list = page.connectionList;
        for (let i = 0; list && i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === "b") {
                const pos = JSON.stringify(d.mapToItem(null, 0, 0)) + "w" + d.width;
                const settled = d.width > 0 && pos === probe.lastRowPos;
                probe.lastRowPos = pos;
                return settled;
            }
        }
        return false;
    }
    function openDetail() {
        const page = probe.pageStack.get(0);
        const list = page.connectionList;
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === "b") { tc.mouseClick(d, 2, 2); return; }
        }
        probe.failures.push("no delegate for b");
    }
    function onlyRulesPage(what) {
        probe.expect(probe.pageStack.depth === 1, what + ": depth " + probe.pageStack.depth);
        const bottom = probe.pageStack.get(0);
        probe.expect(bottom && bottom.objectName === "rulesStub",
                     what + ": bottom page is " + (bottom ? bottom.objectName : "none")
                     + " (a Connections page is still stacked)");
        probe.expect(probe.pageStack.currentItem.objectName === "rulesStub", what + ": rules not on top");
    }

    // Per step: a gate that must hold before the step runs (same index).
    readonly property var gates: ({
        1: () => probe.connectionsReady(),
        3: () => probe.connectionsReady(),
        5: () => probe.connectionsReady()
    })
    readonly property var steps: [
        function () {
            connModel.setGroupedMode(false);
            connModel.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                { id: "b", process: "/usr/bin/curl", processPath: "/usr/bin/curl", dstHost: "b.example.com",
                  dstIp: "203.0.113.7", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1,
                  matchedRule: "my-rule" }] }));
        },
        function () {
            probe.openDetail();
            probe.expect(probe.pageStack.depth === 2, "detail page not pushed");
            probe.findText(probe.pageStack.currentItem, "Show rule").clicked();
        },
        function () { probe.onlyRulesPage("Show rule"); nav.replaceMain(connectionsComponent); },
        function () {
            probe.expect(probe.pageStack.depth === 1 && probe.pageStack.get(0).objectName === "connectionsPage",
                         "did not return to Connections");
            probe.openDetail();
            probe.expect(probe.pageStack.depth === 2, "detail page not pushed (2)");
            probe.find(probe.pageStack.currentItem, "simulateConnectionButton").clicked();
        },
        function () {
            probe.expect(probe.simulated.length === 1, "no simulate request");
            probe.onlyRulesPage("Simulate");
            nav.replaceMain(connectionsComponent);
        },
        function () {
            // Drawer navigation with the detail page open.
            probe.openDetail();
            probe.expect(probe.pageStack.depth === 2, "detail page not pushed (3)");
            nav.replaceMain(rulesStub);
        },
        function () { probe.onlyRulesPage("drawer navigation"); }
    ]

    Timer {
        interval: 250
        running: true
        repeat: true
        onTriggered: {
            if (probe.inStep) return;
            if (probe.step >= probe.steps.length) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("inspector navigation probe: " + probe.failures.join("; "));
                    }
                    console.warn("INSPECTOR_PANEL_NAVIGATION_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
                return;
            }
            const gate = probe.gates[probe.step];
            if (gate && !gate()) {
                // A stuck gate fails the probe after 40 ticks (10 s); it never hangs it.
                if (++probe.stalls > 40) {
                    probe.failures.push("step " + probe.step + " never ready");
                    probe.step = probe.steps.length;
                }
                return;
            }
            probe.stalls = 0;
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
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains("TypeError")
                || line.contains("not placed in the graphics scene")
                || line.contains("DelegateModel::cancel")
        })
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "inspector panel (navigation) probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
