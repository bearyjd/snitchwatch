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
    width: 1400
    height: 900
    property var failures: []
    property int phase: 0
    property var menuRow: null

    QtObject {
        id: feedStub
        property bool ok: true
        property var sent: []
        function appBoundRulesFor(rowId) { return true; }
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {
            sent = sent.concat([rowId + ":" + choice]);
            return true;
        }
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
    function header(level, key) {
        const list = page.connectionList;
        for (let i = 0; i < list.count; ++i) {
            const item = list.itemAtIndex(i);
            if (item && item.isGroupHeader && item.groupLevel === level && item.groupKey === key)
                return item;
        }
        throw new Error("missing header " + level + ":" + key);
    }
    function named(item, name) {
        if (item.objectName === name) return item;
        const children = item.children || [];
        for (const child of children) {
            const found = probe.named(child, name);
            if (found) return found;
        }
        return null;
    }
    function pending(id, process, host) {
        const r = probe.row(id, host);
        r.process = process;
        r.processPath = "/usr/bin/" + process;
        r.dstIp = "1.1.1.1";
        r.action = null;
        return r;
    }
    function expectSent(ids, choice) {
        const expected = ids.map(id => id + ":" + choice).sort();
        probe.expect(JSON.stringify(feedStub.sent.slice().sort()) === JSON.stringify(expected),
                     "wrong bulk scope: " + JSON.stringify(feedStub.sent) + "; expected " + JSON.stringify(expected));
        feedStub.sent = [];
    }
    function rightClick(level, key) {
        probe.menuRow = probe.header(level, key);
        const expanded = probe.menuRow.expanded;
        tc.mouseClick(probe.menuRow, 2, 2, Qt.RightButton);
        probe.expect(probe.menuRow.actions.menu.visible, "right click did not open actions");
        probe.expect(probe.menuRow.expanded === expanded, "right click expanded/collapsed group");
    }
    function clickPending(allow) {
        const item = probe.menuRow.actions.menu.itemAt(allow ? 0 : 1);
        tc.mouseClick(item, item.width / 2, item.height / 2);
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
                    model.applyServerMessageJson(JSON.stringify({ action: "clearConnectionRows" }));
                    model.setGroupedMode(true);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:www", "app", "www.example.com"),
                        probe.pending("s:api", "app", "api.example.com"),
                        probe.pending("s:other", "app", "other.net"),
                        probe.pending("s:another-app", "another", "www.example.com")
                    ] }));
                } else if (probe.phase === 4) {
                    probe.rightClick(0, "/usr/bin/app");
                } else if (probe.phase === 5) {
                    probe.clickPending(false);
                    probe.expectSent(["s:www", "s:api", "s:other"], "deny");
                    // Context and inline buttons share the duplicate-submit guard.
                    probe.menuRow.decideOnce("allow", true);
                    probe.expectSent([], "allow");
                    model.toggleProcessGroup("/usr/bin/app");
                } else if (probe.phase === 7) {
                    probe.menuRow = probe.header(1, "example.com");
                    const handle = probe.named(probe.menuRow, "groupActionsButton");
                    probe.expect(handle !== null, "group Actions handle missing");
                    tc.mouseClick(handle, handle.width / 2, handle.height / 2);
                    probe.expect(probe.menuRow.actions.menu.visible, "Actions handle did not open menu");
                } else if (probe.phase === 8) {
                    probe.clickPending(true);
                    probe.expectSent(["s:www", "s:api"], "allow");
                    model.toggleDomainGroup("/usr/bin/app", "example.com");
                } else if (probe.phase === 10) {
                    probe.rightClick(2, "www.example.com");
                } else if (probe.phase === 11) {
                    probe.clickPending(false);
                    probe.expectSent(["s:www"], "deny");
                    model.setFilterQuery("www.example.com");
                } else if (probe.phase === 13) {
                    // A filtered header still acts on its complete subtree.
                    const domain = probe.header(1, "example.com");
                    domain.submitted = false; // Stub feed does not apply verdict updates.
                    probe.rightClick(1, "example.com");
                } else if (probe.phase === 14) {
                    probe.clickPending(false);
                    probe.expectSent(["s:www", "s:api"], "deny");
                    page.submitGroupVerdict("/usr/bin/app", 0, "", "", "allow", "old:");
                    probe.expectSent([], "allow");
                    // Recycled/changed targets must dismiss the old menu.
                    probe.rightClick(1, "example.com");
                    probe.menuRow.sourceSession = "replacement:";
                    probe.expect(!probe.menuRow.actions.menu.visible,
                                 "menu survived a source-session change");
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
