//! The Connections list (r15 group C), on a real `ConnectionsPage`:
//!
//!   * #114.1: the empty placeholder is drawn while no row exists and hidden
//!     once one does;
//!   * #55: repeated daemon events of one flow and verdict show as ONE row
//!     with a count and last-seen time; a pending prompt of the same flow
//!     and an event with a different verdict stay rows of their own;
//!   * #55: after "From now" a repeat starts a new row instead of hiding in
//!     one from before the cut.
//!
//! One probe, steps run one per timer tick. Failures are collected and
//! thrown together. Headless: `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_collapse_probe.qml";
const DONE_MARKER: &str = "CONNECTIONS_COLLAPSE_PROBE_DONE";

#[test]
fn repeated_events_show_as_one_row_and_the_empty_list_says_so() {
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
    height: 700
    property var failures: []
    property int step: 0

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) { return true; }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, action, at) {
        return { id: id, process: "python3", processPath: "/usr/bin/python3",
                 dstHost: "example.com", dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp",
                 direction: "outgoing", action: action, bytesSent: 0, bytesReceived: 0,
                 startedAtMs: at, matchedRule: action === null ? null : "deny-python" };
    }
    function insert(rows) {
        model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: rows }));
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
        const list = page.connectionList;
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === rowId) return d;
        }
        return null;
    }
    function repeatText(rowId) {
        const label = probe.find(probe.delegateFor(rowId), "repeatLabel");
        return label && label.visible ? label.text : "";
    }
    // Recent enough that "From now" (now) is after them only for the rows
    // inserted before it.
    readonly property real t0: Date.now() - 60000

    property var steps: [
        function () {
            model.setGroupedMode(false);
            const placeholder = probe.find(page, "connectionsEmptyPlaceholder");
            probe.expect(placeholder !== null, "no empty placeholder");
            probe.expect(placeholder && placeholder.visible, "empty list: placeholder not visible");
            probe.insert([probe.row("event-1", "deny", probe.t0),
                          probe.row("event-2", "deny", probe.t0 + 1000),
                          probe.row("ask-3", null, probe.t0 + 1500)]);
            probe.insert([probe.row("event-4", "deny", probe.t0 + 3000),
                          probe.row("event-5", "allow", probe.t0 + 3500)]);
        },
        function () {
            const placeholder = probe.find(page, "connectionsEmptyPlaceholder");
            probe.expect(placeholder && !placeholder.visible, "rows listed: placeholder still visible");
            probe.expect(page.connectionList.count === 3,
                         "expected event-1, ask-3, event-5; count " + page.connectionList.count);
            probe.expect(probe.delegateFor("event-2") === null && probe.delegateFor("event-4") === null,
                         "a repeat got its own row");
            const pending = probe.delegateFor("ask-3");
            probe.expect(pending && pending.pending, "the pending prompt is not its own row");
            probe.expect(probe.delegateFor("event-5") !== null, "the allowed event merged into the denied row");
            const expected = "×3 · last seen " + Qt.formatTime(new Date(probe.t0 + 3000), "hh:mm:ss");
            probe.expect(probe.repeatText("event-1") === expected,
                         "repeat label: '" + probe.repeatText("event-1") + "' vs '" + expected + "'");
            probe.expect(probe.repeatText("event-5") === "", "a single row shows a count");
            probe.expect(probe.repeatText("ask-3") === "", "a pending row shows a count");
            model.setFromNow(true);
            model.setFromNow(false);
            probe.insert([probe.row("event-6", "deny", probe.t0 + 4000)]);
        },
        function () {
            probe.expect(probe.delegateFor("event-6") !== null,
                         "a repeat after From now hid in a row from before it");
            probe.expect(probe.repeatText("event-1").indexOf("×3 ") === 0,
                         "From now changed the earlier count: " + probe.repeatText("event-1"));
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
                        throw new Error("connections collapse probe: " + probe.failures.join("; "));
                    }
                    console.warn("CONNECTIONS_COLLAPSE_PROBE_DONE");
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
        "connections collapse probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
