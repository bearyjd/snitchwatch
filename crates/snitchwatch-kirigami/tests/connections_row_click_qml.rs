//! A click on a leaf connection row must open the inspector for that row and
//! make it the list's current item. The row's click handler lives in a child
//! `MouseArea`, where the `ListView` attached properties do not exist (they
//! are only defined on the delegate root); an unqualified `ListView.view` there
//! is null, the handler throws, and `openInspector` never runs. Drives the real
//! `ConnectionsPage` + real `ConnectionRow` delegate with a QtTest `mouseClick`.
//! A throw inside `ConnectionRow.qml` is reported against that file's URL, not
//! the probe's, so the Rust side also fails on any TypeError in the capture.
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_row_click_probe.qml";
const DONE_MARKER: &str = "ROW_CLICK_PROBE_DONE";

#[test]
fn clicking_a_leaf_row_opens_the_inspector_and_selects_it() {
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
    width: 800
    height: 600
    property var failures: []
    property int phase: 0

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {}
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }
    TestCase { id: tc; name: "rowclick"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function findList(item) {
        if (typeof item.itemAtIndex === "function" && item.currentIndex !== undefined) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findList(kids[i]);
            if (f) return f;
        }
        return null;
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    model.setGroupedMode(false);
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com")]
                    }));
                } else if (probe.phase === 2) {
                    const list = probe.findList(page);
                    probe.expect(list !== null, "no ListView found under the page");
                    if (list) {
                        probe.expect(list.count === 2, "rows: " + list.count);
                        const item = list.itemAtIndex(1);
                        probe.expect(item !== null, "no delegate at index 1");
                        if (item) {
                            tc.mouseClick(item, 2, 2);
                            probe.expect(page.inspectId === item.rowId,
                                         "inspector did not open for the clicked row: '" + page.inspectId + "'");
                            probe.expect(list.currentIndex === 1,
                                         "currentIndex not set: " + list.currentIndex);
                        }
                    }
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
                        throw new Error("row click probe: " + probe.failures.join("; "));
                    }
                    console.warn("ROW_CLICK_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
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
        "clicking a leaf row did not open the inspector:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
