//! Auto-select in GROUPED mode. Two pending connections arriving in the same
//! tick, under a program/domain/host the user has open, must select the OLDEST
//! pending row (never a group header, never the topmost row: groups list the
//! most recent first). The page queues row ids, not list indices, because later
//! inserts shift indices. One probe, one step per timer tick. Headless:
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/inspector_panel_grouped_probe.qml";
const DONE_MARKER: &str = "GROUPED_AUTOSELECT_PROBE_DONE";

#[test]
fn grouped_auto_select_picks_the_oldest_pending_row() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1200
    height: 600
    property var failures: []
    property int step: 0

    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: ConnectionsModel { id: model }
    }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, action, process, startedAtMs) {
        return { id: id, process: process, processPath: "/usr/bin/" + process, dstHost: "x.example.com",
                 dstIp: "203.0.113.7", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: startedAtMs, matchedRule: null };
    }
    // A model row need not have a delegate until it enters the viewport.
    function materialize(index) {
        const list = page.connectionList;
        list.positionViewAtIndex(index, ListView.Contain);
        list.forceLayout();
        return list.itemAtIndex(index);
    }
    // Visit every header, including those outside the initial viewport.
    // Open collapsed headers at `depth` (0 program, 1 domain, 2 host).
    function expandDepth(depth) {
        const list = page.connectionList;
        list.forceLayout();
        const todo = [];
        for (let i = 0; i < list.count; i++) {
            const d = probe.materialize(i);
            if (d && d.isGroupHeader && d.groupLevel === depth && !d.expanded) {
                todo.push({ key: d.groupKey, parent: d.groupParentKey, process: d.groupProcessKey });
            }
        }
        for (const t of todo) {
            if (depth === 0) model.toggleProcessGroup(t.key);
            else if (depth === 2) model.toggleHostGroup(t.process, t.parent, t.key);
            else model.toggleDomainGroup(t.parent, t.key);
        }
    }

    readonly property var steps: [
        function () {
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                rows: [probe.row("old", "allow", "curl", 1),
                       probe.row("oldw", "allow", "wget", 1)] }));
        },
        function () { probe.expandDepth(0); },
        function () { probe.expandDepth(1); },
        function () { probe.expandDepth(2); },
        function () {
            const list = page.connectionList;
            list.forceLayout();
            let leaves = 0;
            for (let i = 0; i < list.count; i++) {
                const d = probe.materialize(i);
                if (d && !d.isGroupHeader) leaves++;
            }
            probe.expect(leaves === 2 && !page.inspecting, "setup: leaves " + leaves);
            for (const id of ["old", "oldw"]) {
                const index = model.visibleRowIndex(id);
                const d = probe.materialize(index);
                probe.expect(index >= 0 && d && d.rowId === id && !d.isGroupHeader,
                             "setup did not render the expanded leaf " + id);
            }
            list.positionViewAtBeginning();
            list.forceLayout();
            // Two arrivals in the same tick: pA first, in the lower wget group,
            // then pB (newer) in the curl group above it. pB's row pushes pA's
            // down, so the index recorded for pA is stale by the time the page
            // runs; the oldest pending row is still pA.
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                rows: [probe.row("pA", null, "wget", 1)] }));
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                rows: [probe.row("pB", null, "curl", 2)] }));
            probe.expect(list.itemAtIndex(model.visibleRowIndex("pA")) === null,
                         "oldest pending row must start outside the viewport");
        },
        function () {
            probe.expect(page.inspecting && page.inspectId === "pA",
                         "grouped auto-select chose '" + page.inspectId + "', not the oldest pending row");
            const list = page.connectionList;
            const d = list.currentItem;
            probe.expect(list.currentIndex === model.visibleRowIndex("pA")
                         && d && d.rowId === "pA" && !d.isGroupHeader,
                         "auto-select did not select the oldest pending leaf");
            probe.expect(list.itemAtIndex(list.currentIndex) === d,
                         "auto-selected leaf was not brought into the viewport");
        }
    ]

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            if (probe.step >= probe.steps.length) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("grouped auto-select probe: " + probe.failures.join("; "));
                    }
                    console.warn("GROUPED_AUTOSELECT_PROBE_DONE");
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
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains("TypeError")
                || line.contains("DelegateModel::cancel: index out range")
        })
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "grouped auto-select probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
