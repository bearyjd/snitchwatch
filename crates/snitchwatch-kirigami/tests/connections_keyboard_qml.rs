//! Q01 (first slice): the Connections page by keyboard alone, with REAL key
//! events (QtTest `keyClick` on the window), never by calling handlers.
//!   * Tab reaches the list; arrows select a row.
//!   * The Menu key and Shift+F10 open the row's context menu; arrows reach
//!     every action in it, Enter fires one, Escape closes it, and focus goes
//!     back to where it was.
//!   * From a selected group header, Tab goes to that header's bulk actions
//!     (Allow all, Deny all) in order, Shift+Tab goes back, Space fires.
//!
//! Each phase waits on a readiness gate (the #237 pattern in
//! `connections_row_click_qml.rs`) instead of a fixed tick, so it holds under
//! load. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_keyboard_probe.qml";
const DONE_MARKER: &str = "KEYBOARD_PROBE_DONE";

const QML: &str = r#"
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
    property int stalls: 0
    readonly property int maxStalls: 200
    property var geo: ({})
    property var focusBefore: null
    property int rowsBefore: 0
    readonly property var actions: page.connectionActions
    readonly property var menu: page.connectionActions.menu
    readonly property var list: page.connectionList

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
    TestCase { id: tc; name: "keyboard"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function pending(id, host) {
        return { id: id, process: "app", processPath: "/usr/bin/app", dstHost: host,
                 dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function stable(key, item) {
        if (!item || !item.visible || !(item.width > 0) || !(item.height > 0)) {
            probe.geo[key] = "";
            return false;
        }
        const g = item.mapToItem(null, 0, 0);
        const geometry = [g.x, g.y, item.width, item.height].join(",");
        const same = geometry === probe.geo[key];
        probe.geo[key] = geometry;
        return same;
    }
    function settled() { return probe.menu.opened || !probe.menu.visible; }
    function menuOpen() { return probe.settled() && probe.menu.opened; }
    function menuClosed() { return probe.settled() && !probe.menu.visible; }
    function focused() { return probe.activeFocusItem; }
    function current() { return probe.menu.itemAt(probe.menu.currentIndex); }
    // Every action the open menu can fire: shown and enabled items with a name.
    function actionable() {
        const names = [];
        for (let i = 0; i < probe.menu.count; ++i) {
            const item = probe.menu.itemAt(i);
            if (item && item.objectName && item.visible && item.enabled && item.height > 0)
                names.push(item.objectName);
        }
        return names.sort();
    }
    // Press Down through the whole menu, collecting what gets highlighted.
    function reachable() {
        const seen = {};
        for (let i = 0; i < probe.menu.count * 2 + 2; ++i) {
            tc.keyClick(Qt.Key_Down);
            const item = probe.current();
            if (item && item.objectName) seen[item.objectName] = true;
        }
        return Object.keys(seen).sort();
    }
    // Up to the top (the menu does not wrap), then Down to `name`.
    function highlight(name) {
        for (let i = 0; i < probe.menu.count + 1; ++i) tc.keyClick(Qt.Key_Up);
        for (let i = 0; i < probe.menu.count * 2 + 2; ++i) {
            const item = probe.current();
            if (item && item.objectName === name) return true;
            tc.keyClick(Qt.Key_Down);
        }
        return false;
    }
    function expectReachesEveryAction(what) {
        const expected = probe.actionable();
        const got = probe.reachable();
        probe.expect(expected.length > 0, what + ": no actions in the menu");
        const missing = expected.filter(n => got.indexOf(n) < 0);
        probe.expect(missing.length === 0, what + ": arrows never reach " + JSON.stringify(missing)
                     + " (reached " + JSON.stringify(got) + ")");
    }
    function headerAt(index) {
        const item = probe.list.itemAtIndex(index);
        return item && item.isGroupHeader ? item : null;
    }
    function text(item) { return item && item.text !== undefined ? String(item.text) : ""; }
    function inside(item, ancestor) {
        for (let it = item; it; it = it.parent) if (it === ancestor) return true;
        return false;
    }
    // A bulk button of `header` has focus, and its text starts with `prefix`.
    function onBulk(header, prefix) {
        return probe.inside(probe.focused(), header) && probe.text(probe.focused()).indexOf(prefix) === 0;
    }

    function gate(phase) {
        switch (phase) {
        case 1: return probe.list.count === 2 && probe.stable("r1", probe.list.itemAtIndex(1))
            ? "" : "two rows";
        case 4: case 8: case 13: return probe.menuOpen() ? "" : "menu open";
        case 5: case 9: case 14: return probe.menuClosed() ? "" : "menu closed";
        case 11: return probe.stable("h1", probe.headerAt(1)) ? "" : "second group header";
        default: return probe.settled() ? "" : "popup transition";
        }
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const waiting = probe.gate(probe.phase);
                if (waiting !== "") {
                    if (++probe.stalls < probe.maxStalls) return;
                    throw new Error("never ready for " + waiting);
                }
                probe.stalls = 0;
                if (probe.phase === 0) {
                    probe.requestActivate();
                    model.setGroupedMode(false);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                        rows: [probe.pending("a", "x.example.com"), probe.pending("b", "y.example.com")] }));
                } else if (probe.phase === 1) {
                    // Tab, and only Tab, brings focus to the list.
                    for (let i = 0; i < 40 && !probe.list.activeFocus; ++i) tc.keyClick(Qt.Key_Tab);
                    probe.expect(probe.list.activeFocus, "Tab never reached the connections list");
                } else if (probe.phase === 2) {
                    // A pending row may already be selected; arrows go to the top.
                    tc.keyClick(Qt.Key_Down);
                    for (let i = 0; i < 3; ++i) tc.keyClick(Qt.Key_Up);
                    probe.expect(probe.list.currentIndex === 0, "arrows did not select the first row: "
                                 + probe.list.currentIndex);
                } else if (probe.phase === 3) {
                    probe.focusBefore = probe.focused();
                    tc.keyClick(Qt.Key_Menu);
                } else if (probe.phase === 4) {
                    probe.expect(probe.actions.target && probe.actions.target.rowId === "a",
                                 "the Menu key opened the wrong row's menu");
                    probe.expectReachesEveryAction("leaf menu");
                    probe.expect(probe.highlight("allowPendingAction"), "cannot highlight Allow");
                    tc.keyClick(Qt.Key_Return);
                } else if (probe.phase === 5) {
                    probe.expect(feedStub.sent.indexOf("a:allow") >= 0,
                                 "Enter on Allow sent nothing: " + JSON.stringify(feedStub.sent));
                    probe.expect(probe.focused() === probe.focusBefore,
                                 "focus did not return after Enter: " + probe.focused());
                    feedStub.sent = [];
                } else if (probe.phase === 6) {
                    tc.keyClick(Qt.Key_Down);
                    probe.expect(probe.list.currentIndex === 1, "Down did not move to the second row");
                } else if (probe.phase === 7) {
                    probe.focusBefore = probe.focused();
                    tc.keyClick(Qt.Key_F10, Qt.ShiftModifier);
                } else if (probe.phase === 8) {
                    probe.expect(probe.actions.target && probe.actions.target.rowId === "b",
                                 "Shift+F10 opened the wrong row's menu");
                    tc.keyClick(Qt.Key_Escape);
                } else if (probe.phase === 9) {
                    probe.expect(probe.focused() === probe.focusBefore,
                                 "Escape did not return focus to the row: " + probe.focused());
                    probe.expect(probe.list.currentIndex === 1, "Escape moved the selection");
                    probe.expect(feedStub.sent.length === 0, "Escape sent a verdict");
                } else if (probe.phase === 10) {
                    // A second program, so the selected header is not the
                    // first row (whose buttons plain Tab order reaches anyway).
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                        rows: [Object.assign(probe.pending("c", "z.example.org"),
                                             { process: "other", processPath: "/usr/bin/other" })] }));
                    model.setGroupedMode(true);
                } else if (probe.phase === 11) {
                    probe.expect(probe.list.activeFocus, "the list lost focus when the view was grouped");
                    tc.keyClick(Qt.Key_Down);
                    for (let i = 0; i < 5; ++i) tc.keyClick(Qt.Key_Up);
                    tc.keyClick(Qt.Key_Down);
                    probe.expect(probe.list.currentIndex === 1, "arrows did not select the second header: "
                                 + probe.list.currentIndex);
                    const target = probe.headerAt(1);
                    // The bulk bar, in order, from the selected header.
                    tc.keyClick(Qt.Key_Tab);
                    probe.expect(probe.onBulk(target, "Allow all"),
                                 "Tab from a header did not reach its Allow all: " + probe.text(probe.focused()));
                    tc.keyClick(Qt.Key_Tab);
                    probe.expect(probe.onBulk(target, "Deny all"),
                                 "the next Tab did not reach its Deny all: " + probe.text(probe.focused()));
                    tc.keyClick(Qt.Key_Backtab, Qt.ShiftModifier);
                    probe.expect(probe.onBulk(target, "Allow all"),
                                 "Shift+Tab did not go back to Allow all: " + probe.text(probe.focused()));
                    tc.keyClick(Qt.Key_Tab);
                    tc.keyClick(Qt.Key_Space);
                    // "a" was answered by the menu above, so only "b" is left
                    // under app; "c" is the other program's.
                    const expected = target.groupKey === "/usr/bin/app" ? ["b:deny"] : ["c:deny"];
                    probe.expect(JSON.stringify(feedStub.sent) === JSON.stringify(expected),
                                 "Space on Deny all sent " + JSON.stringify(feedStub.sent)
                                 + ", expected " + JSON.stringify(expected));
                    feedStub.sent = [];
                } else if (probe.phase === 12) {
                    // The header's menu, from its focused bulk button.
                    probe.focusBefore = probe.focused();
                    tc.keyClick(Qt.Key_Menu);
                } else if (probe.phase === 13) {
                    probe.expect(probe.actions.target && probe.actions.target.header,
                                 "the Menu key did not open the header's menu");
                    probe.expectReachesEveryAction("header menu");
                    probe.expect(probe.highlight("groupConnectionsAction"), "cannot highlight Expand / collapse");
                    probe.rowsBefore = probe.list.count;
                    tc.keyClick(Qt.Key_Return);
                } else if (probe.phase === 14) {
                    probe.expect(probe.list.count > probe.rowsBefore,
                                 "Enter on Expand / collapse did nothing: " + probe.list.count);
                    probe.expect(probe.focused() === probe.focusBefore,
                                 "focus did not return to the bulk button: " + probe.focused());
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
                        throw new Error("keyboard probe: " + probe.failures.join("; "));
                    }
                    console.warn("KEYBOARD_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn the_connections_page_works_by_keyboard_alone() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(QML), &QUrl::from(PROBE_URL));
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
    let bad: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(bad.is_empty(), "keyboard probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
