//! Inspector side panel (monitor-tree step 5), narrow layout. A real
//! `ConnectionsPage` on a Kirigami.ApplicationWindow's page stack, narrower than
//! the panel threshold, opens the inspector as a pushed page. Covers:
//!
//!   * a REAL mouse click on a row pushes a ConnectionDetailPage on the page
//!     stack (not a sheet) that shows the row's host, and the side column is
//!     not instantiated;
//!   * the pushed page is the one `makeRuleSheet`/`decisionSheet` resolve to;
//!   * auto-select of a new pending row never pushes a page;
//!   * dismissing (a finished action / close) pops the page and clears the
//!     selection; widening the window pops it and shows the column.
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

const PROBE_URL: &str = "qrc:/inspector_panel_narrow_probe.qml";
const DONE_MARKER: &str = "INSPECTOR_PANEL_NARROW_PROBE_DONE";

#[test]
fn a_narrow_window_pushes_the_panel_as_a_page() {
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
    property var page: null

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) { return true; }
    }
    ConnectionsModel { id: model }
    pageStack.initialPage: ConnectionsPage {
        model: model
        bridgeFeed: feedStub
    }
    Component {
        id: stubComponent
        Kirigami.Page {}
    }
    TestCase { id: tc; name: "panel"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host, action) {
        return { id: id, process: "/usr/bin/curl", processPath: "/usr/bin/curl",
                 dstHost: host, dstIp: "203.0.113.7", dstPort: 443, protocol: "tcp",
                 direction: "outgoing", action: action, bytesSent: 0, bytesReceived: 0,
                 startedAtMs: 1, matchedRule: null };
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
    function delegateFor(rowId) {
        const list = probe.page.connectionList;
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === rowId) return d;
        }
        return null;
    }

    // tc.mouseClick/keyClick run the event loop; under load the next tick can
    // fire inside a step and re-run it before `step` advances.
    property bool inStep: false
    readonly property var steps: [
        function () {
            probe.page = probe.pageStack.get(0);
            probe.expect(probe.page.compact, "a 600px window must be compact");
            probe.expect(probe.page.activePanel === null, "side column instantiated while compact");
            model.setGroupedMode(false);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("a", "a.example.com", "allow")] }));
        },
        function () {
            const d = probe.delegateFor("a");
            probe.expect(d !== null, "no delegate for a");
            if (d) tc.mouseClick(d, 2, 2);
            probe.expect(probe.pageStack.depth === 2, "no page pushed, depth " + probe.pageStack.depth);
            const detail = probe.pageStack.currentItem;
            probe.expect(probe.page.detailPage === detail && detail !== probe.page,
                         "the pushed page is not the page's detailPage");
            const host = probe.find(detail, "inspectorHost");
            probe.expect(host !== null && host.text === "a.example.com",
                         "pushed page host: " + (host ? host.text : "none"));
            probe.expect(probe.page.makeRuleSheet === detail.makeRuleSheet
                         && probe.page.decisionSheet !== null, "sheets not routed to the pushed page");
            probe.expect(!probe.find(probe.page, "inspectorPlaceholder"), "side column still present");
        },
        function () {
            // A new pending row is auto-selected but must not push a second page.
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("p", "p.example.com", null)] }));
        },
        function () {
            probe.expect(probe.pageStack.depth === 2, "depth after a pending row: " + probe.pageStack.depth);
            probe.page.dismissInspector();
        },
        function () {
            probe.expect(probe.pageStack.depth === 1, "dismiss did not pop: " + probe.pageStack.depth);
            probe.expect(!probe.page.inspecting && probe.page.detailPage === null, "selection kept after dismiss");
            // Auto-select must not push a page either.
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("q", "q.example.com", null)] }));
        },
        function () {
            probe.expect(probe.pageStack.depth === 1, "auto-select pushed a page: " + probe.pageStack.depth);
            const d = probe.delegateFor("a");
            if (d) tc.mouseClick(d, 2, 2);
            probe.expect(probe.pageStack.depth === 2, "second click did not push");
            probe.width = 1300;
        },
        function () {
            probe.expect(!probe.page.compact, "1300px window still compact: page width " + probe.page.width + " window " + probe.width + " cw " + probe.pageStack.columnView.columnResizeMode);
            probe.expect(probe.pageStack.depth === 1, "widening did not pop: " + probe.pageStack.depth);
            probe.expect(probe.page.activePanel !== null, "no side column after widening");
            probe.expect(probe.page.inspecting, "selection lost on widening");
            probe.width = 600;
        },
        function () {
            // Narrowing with a selection shows it on a pushed page.
            probe.expect(probe.page.compact, "600px window not compact again");
            probe.expect(probe.pageStack.depth === 2 && probe.page.detailPage !== null,
                         "narrowing did not push the selection: " + probe.pageStack.depth);
            probe.page.dismissInspector();
        },
        function () {
            // Keyboard: Down selects without pushing (compact waits for a click),
            // Enter opens the page.
            probe.expect(probe.pageStack.depth === 1, "dismiss did not pop");
            const list = probe.page.connectionList;
            list.forceActiveFocus();
            tc.keyClick(Qt.Key_Down);
            probe.expect(probe.pageStack.depth === 1, "an arrow key pushed a page");
            tc.keyClick(Qt.Key_Return);
        },
        function () {
            probe.expect(probe.pageStack.depth === 2, "Enter did not open the detail page: "
                         + probe.pageStack.depth);
            // Another page pushed on top (the onboarding page on a daemon
            // disconnect) must not end the selection or empty the detail page.
            probe.pageStack.push(stubComponent);
        },
        function () {
            probe.expect(probe.pageStack.depth === 3, "stub not pushed: " + probe.pageStack.depth);
            probe.expect(probe.page.inspecting && probe.page.detailPage !== null,
                         "a page pushed on top dismissed the selection");
            probe.pageStack.pop();
        },
        function () {
            probe.expect(probe.pageStack.depth === 2 && probe.page.inspecting
                         && probe.page.detailPage !== null,
                         "popping the page above left the detail page empty: depth "
                         + probe.pageStack.depth);
            // Back (not the close button): pops the page by the stack's own means.
            probe.pageStack.pop();
        },
        function () {
            // The pop's animation has finished: the selection ended with the page,
            // so auto-select works again (it took the oldest pending row).
            probe.expect(probe.pageStack.depth === 1, "back did not pop");
            probe.expect(!probe.page.inspecting && probe.page.detailPage === null,
                         "Back left the selection active");
            const d = probe.page.connectionList.itemAtIndex(probe.page.connectionList.currentIndex);
            probe.expect(d && d.pending, "Back did not release auto-select (the oldest pending row "
                         + "was not selected)");
        }
    ]

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            if (probe.inStep) return;
            if (probe.step >= probe.steps.length) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("inspector panel narrow probe: " + probe.failures.join("; "));
                    }
                    console.warn("INSPECTOR_PANEL_NARROW_PROBE_DONE");
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
        // Kirigami creates the pages of a bare Component before parenting them.
        .filter(|line| !line.contains("not placed in the graphics scene"))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "inspector panel (narrow) probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
