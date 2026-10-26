//! 0d part 2, on a real `ConnectionsPage`: how many desktop notices of a
//! flow were held back is shown on the flow's newest pending row only, in
//! words of its own (never the collapse "×N" text, never "retried"), and it
//! hides or removes nothing: every row stays listed and the pending count
//! and age behind the banners stay as they were.
//!
//! The count comes from the notice gate through the bridge handles, which
//! a probe has none of, so it is injected with `holdBackNoticesForTests`.
//! One probe, steps run one per timer tick. Headless: `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/held_back_notice_probe.qml";
const DONE_MARKER: &str = "HELD_BACK_NOTICE_PROBE_DONE";

#[test]
fn the_newest_pending_row_says_how_many_notices_were_held_back() {
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
    property var before: null

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
    function row(id, host, at) {
        return { id: id, process: "python3", processPath: "/usr/bin/python3",
                 dstHost: host, dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp",
                 direction: "outgoing", action: null, bytesSent: 0, bytesReceived: 0,
                 startedAtMs: at, matchedRule: null };
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
        const at = model.visibleRowIndex(rowId);
        if (at < 0) return null;
        list.positionViewAtIndex(at, ListView.Contain);
        list.forceLayout();
        for (let i = 0; i < list.count; i++) {
            const d = list.itemAtIndex(i);
            if (d && d.rowId === rowId) return d;
        }
        return null;
    }
    // The held-back note of a row ("" when hidden or missing).
    function heldBack(rowId) {
        const label = probe.find(probe.delegateFor(rowId), "heldBackLabel");
        return label && label.visible ? label.text : "";
    }
    function state() {
        return { count: page.connectionList.count, pending: model.pendingCount,
                 age: model.oldestPendingAgeSecs };
    }
    readonly property real t0: Date.now() - 20000

    property var steps: [
        function () {
            model.setGroupedMode(false);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("ask-1", "example.com", probe.t0),
                probe.row("ask-2", "example.com", probe.t0 + 1000),
                probe.row("ask-3", "example.org", probe.t0 + 2000)] }));
        },
        function () {
            probe.before = probe.state();
            probe.expect(probe.before.count === 3 && probe.before.pending === 3,
                         "setup: " + JSON.stringify(probe.before));
            probe.expect(probe.heldBack("ask-2") === "", "a note before anything was held back");
            model.holdBackNoticesForTests("/usr/bin/python3", "example.com", 3);
        },
        function () {
            const text = probe.heldBack("ask-2");
            probe.expect(text === "notice held back 3 times in the last hour",
                         "newest pending row's note: '" + text + "'");
            probe.expect(text.indexOf("retried") < 0, "the note says retried");
            probe.expect(probe.heldBack("ask-1") === "", "an older row of the flow has the note");
            probe.expect(probe.heldBack("ask-3") === "", "another host has the note");
            const host = probe.find(probe.delegateFor("ask-2"), "hostLabel");
            probe.expect(host && host.text.indexOf("held back") < 0 && host.text.indexOf("×") < 0,
                         "the note is mixed into the host line: " + (host ? host.text : "none"));
            const after = probe.state();
            probe.expect(after.count === probe.before.count && after.pending === probe.before.pending
                         && after.age >= 0,
                         "a held-back notice changed the list or the banner: " + JSON.stringify(after));
            for (const id of ["ask-1", "ask-2", "ask-3"])
                probe.expect(probe.delegateFor(id) !== null, id + " is no longer listed");
            model.holdBackNoticesForTests("/usr/bin/python3", "example.com", 1);
        },
        function () {
            probe.expect(probe.heldBack("ask-2") === "notice held back 1 time in the last hour",
                         "one notice: '" + probe.heldBack("ask-2") + "'");
            // Answered, the newest row no longer waits: the note moves to
            // the flow's newest row that still does.
            model.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [
                Object.assign(probe.row("ask-2", "example.com", probe.t0 + 1000),
                              { action: "allow", matchedRule: "allow-python" })] }));
        },
        function () {
            probe.expect(probe.heldBack("ask-2") === "", "an answered row keeps the note");
            probe.expect(probe.heldBack("ask-1") === "notice held back 1 time in the last hour",
                         "the note did not move to ask-1: '" + probe.heldBack("ask-1") + "'");
            model.holdBackNoticesForTests("/usr/bin/python3", "example.com", 0);
        },
        function () {
            probe.expect(probe.heldBack("ask-1") === "", "a cleared count still shows");
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
                        throw new Error("held back notice probe: " + probe.failures.join("; "));
                    }
                    console.warn("HELD_BACK_NOTICE_PROBE_DONE");
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
        "held back notice probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
