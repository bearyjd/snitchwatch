//! The program > domain > host > connection tree, driven through the real
//! `ConnectionsPage` and `ConnectionRow`. Expanding each tier by clicking its
//! header reveals the next one with the right `groupLevel`, host rows carry
//! their connection count, and a click on a connection row under a host still
//! opens the inspector and selects it (the click handler's `ListView.view`
//! must stay qualified). Any TypeError in the capture fails the test. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_host_row_probe.qml";
const DONE_MARKER: &str = "HOST_ROW_PROBE_DONE";

#[test]
fn expanding_to_a_host_and_clicking_its_connection_opens_the_inspector() {
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
    TestCase { id: tc; name: "hostrow"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    property int hostIndex: -1
    function levels() {
        const list = probe.findList(page);
        const out = [];
        for (let i = 0; i < list.count; ++i) out.push(list.itemAtIndex(i).groupLevel);
        return out.join(",");
    }
    // Click the header at `index` the way a user would.
    function click(index) {
        const list = probe.findList(page);
        const item = list.itemAtIndex(index);
        probe.expect(item !== null && item.isGroupHeader, "no header at " + index);
        if (item) tc.mouseClick(item, 2, 2);
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
                    model.setGroupedMode(true);
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com"),
                               probe.row("c", "y.example.com")]
                    }));
                } else if (probe.phase === 1) {
                    model.setPendingOnly(false);
                } else if (probe.phase === 2) {
                    probe.click(0);
                } else if (probe.phase === 3) {
                    probe.expect(probe.levels() === "0,1", "after program click: " + probe.levels());
                    probe.click(1);
                } else if (probe.phase === 4) {
                    probe.expect(probe.levels() === "0,1,2,2", "after domain click: " + probe.levels());
                    const list = probe.findList(page);
                    const hosts = [2, 3].map(i => list.itemAtIndex(i)).filter(it => it.groupLabel === "y.example.com");
                    probe.expect(hosts.length === 1, "no y.example.com host row");
                    probe.expect(hosts[0].groupTotal === 2, "host count: " + hosts[0].groupTotal);
                    probe.expect(!hosts[0].expanded, "host should start collapsed");
                    probe.hostIndex = hosts[0].index;
                    probe.click(probe.hostIndex);
                } else if (probe.phase === 5) {
                    probe.expect(probe.levels().split(",").length === 6, "after host click: " + probe.levels());
                    const list = probe.findList(page);
                    const hostItem = list.itemAtIndex(probe.hostIndex);
                    probe.expect(hostItem.expanded, "host should be expanded");
                    const item = list.itemAtIndex(probe.hostIndex + 1);
                    probe.expect(item !== null && item.groupLevel === 3 && item.depth === 3,
                                 "no connection row under the host");
                    if (item) {
                        probe.expect(page.inspectId !== item.rowId, "inspector open too early");
                        tc.mouseClick(item, 2, 2);
                        probe.expect(page.inspectId === item.rowId,
                                     "inspector did not open for the clicked row: '" + page.inspectId + "'");
                        probe.expect(list.currentIndex === item.index,
                                     "currentIndex not set: " + list.currentIndex);
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
                        throw new Error("host row probe: " + probe.failures.join("; "));
                    }
                    console.warn("HOST_ROW_PROBE_DONE");
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
        "the host tree did not behave:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
