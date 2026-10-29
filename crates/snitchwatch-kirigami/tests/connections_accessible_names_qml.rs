//! Q01 guard: every interactive control on the Connections page carries a
//! meaningful `Accessible.name`. It walks the REAL object tree at runtime
//! (window content and the popup overlay; the title row is hosted beside the
//! page, since only a page stack draws it), with the page in the states that
//! show the most controls: grouped and expanded, pending leaves, the
//! inspector open, the exception-first bar, and each context menu open. A
//! control added later without a name fails here. Each phase waits on a
//! readiness gate (#237). Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;

const PROBE_URL: &str = "qrc:/connections_accessible_names_probe.qml";
const DONE_MARKER: &str = "ACCESSIBLE_NAMES_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");

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
    // What each decision selector must be called after the change.
    property var want: ({})
    // @a11y-guard
    readonly property string rec: "snitchwatch-default-app"
    readonly property var actions: page.connectionActions
    readonly property var menu: page.connectionActions.menu
    readonly property var list: page.connectionList

    QtObject {
        id: feedStub
        property bool ok: true
        function appBoundRulesFor(rowId) { return true; }
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) { return true; }
    }
    // The page's title row (its `titleDelegate`, which only a page stack
    // draws): the search field and the Grouped switch.
    ConnectionsHeader {
        id: header
        width: parent.width
        model: model
        title: "Connections"
    }
    ConnectionsPage {
        id: page
        anchors { top: header.bottom; left: parent.left; right: parent.right; bottom: parent.bottom }
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }
    TestCase { id: tc; name: "a11ynames"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host, action, rule) {
        return { id: id, process: "app", processPath: "/usr/bin/app", dstHost: host,
                 dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: 1,
                 matchedRule: rule };
    }
    function send(msg) { model.applyServerMessageJson(JSON.stringify(msg)); }
    function settled() { return probe.menu.opened || !probe.menu.visible; }
    // A shown decision selector, by objectName.
    function selector(key) {
        const box = probe.find(page, it => it.objectName === key && it.visible && it.enabled);
        probe.expect(box !== null, "no visible, enabled " + key);
        return box;
    }
    function headerOrNull(level, key) {
        for (let i = 0; i < probe.list.count; ++i) {
            const item = probe.list.itemAtIndex(i);
            if (item && item.isGroupHeader && item.groupLevel === level && item.groupKey === key)
                return item;
        }
        return null;
    }
    function leafOrNull(id) {
        const index = model.visibleRowIndex(id);
        return index >= 0 ? probe.list.itemAtIndex(index) : null;
    }

    function gate(phase) {
        switch (phase) {
        case 1: return probe.stable("h0", probe.headerOrNull(0, "/usr/bin/app")) ? "" : "app header";
        case 2: return probe.stable("h1", probe.headerOrNull(1, "example.com")) ? "" : "domain header";
        case 3: return probe.stable("h2", probe.headerOrNull(2, "www.example.com")) ? "" : "host header";
        case 4: return probe.stable("leaf", probe.leafOrNull("w")) ? "" : "pending leaf";
        case 6: case 8: return probe.settled() && probe.menu.opened ? "" : "menu open";
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
                    // A snapshot with a handled row, so the exception-first
                    // bar shows Show all, Expand and Dismiss as well.
                    probe.send({ action: "clearConnectionRows" });
                    probe.send({ action: "insertConnectionRows", rows: [
                        probe.row("w", "www.example.com", null, null),
                        probe.row("a", "api.example.com", null, null),
                        probe.row("h", "ntp.example.com", "allow", probe.rec)] });
                    probe.send({ action: "setRules", rules: [
                        { name: probe.rec, enabled: true, action: "allow", toggleable: true }] });
                } else if (probe.phase === 1) {
                    model.toggleProcessGroup("/usr/bin/app");
                } else if (probe.phase === 2) {
                    model.toggleDomainGroup("/usr/bin/app", "example.com");
                } else if (probe.phase === 3) {
                    model.toggleHostGroup("/usr/bin/app", "example.com", "www.example.com");
                } else if (probe.phase === 4) {
                    const leaf = probe.leafOrNull("w");
                    tc.mouseClick(leaf, 2, 2);
                } else if (probe.phase === 5) {
                    probe.expect(model.backgroundCount === 1, "the bar is not showing: " + model.backgroundCount);
                    probe.sweep("page");
                    probe.actions.open(probe.headerOrNull(1, "example.com"), 0, 0);
                } else if (probe.phase === 6) {
                    probe.sweep("group menu");
                    probe.menu.close();
                } else if (probe.phase === 7) {
                    probe.actions.open(probe.leafOrNull("w"), 0, 0);
                } else if (probe.phase === 8) {
                    probe.sweep("connection menu");
                    probe.menu.close();
                    probe.expect(probe.checked > 20, "the guard saw too few controls: " + JSON.stringify(probe.seen));
                    // The names identified as needed, exactly.
                    const expected = ["Scope: This host only", "Duration: Forever", "Actions for this group",
                        "Grouped", "Pending only", "Denied only", "From now", "Full list", "Show all",
                        "Expand", "Dismiss", "Allow all (1)", "Deny all (1)", "Copy host",
                        "Expand / collapse group", "Group connections by app"];
                    const absent = expected.filter(n => !probe.names[n]);
                    probe.expect(absent.length === 0, "expected names not found: " + JSON.stringify(absent));
                    probe.expect(Object.keys(probe.names).some(n => n.indexOf("Search connections.") === 0),
                                 "the search field lost its name");
                    // A choice in each selector, found by objectName (never by
                    // its name): both names must follow the new values.
                    for (const [key, label] of [["scopeSelector", "Scope: "], ["durationSelector", "Duration: "]]) {
                        const box = probe.selector(key);
                        if (!box) continue;
                        const before = String(box.Accessible.name);
                        box.currentIndex = (box.currentIndex + 1) % box.count;
                        probe.want[key] = label + box.textAt(box.currentIndex);
                        probe.expect(probe.want[key] !== before, key + " has a single choice");
                        probe.expect(String(box.Accessible.name) === probe.want[key],
                                     key + " is called '" + box.Accessible.name + "' after the choice, not '"
                                     + probe.want[key] + "'");
                    }
                    probe.send({ action: "insertConnectionRows", rows: [probe.row("n", "new.example.org", null, null)] });
                } else if (probe.phase === 9) {
                    probe.names = {};
                    probe.sweep("after a change");
                    for (const key of ["scopeSelector", "durationSelector"]) {
                        const box = probe.selector(key);
                        probe.expect(!!box && String(box.Accessible.name) === probe.want[key],
                                     key + " after the model change: '" + (box && box.Accessible.name)
                                     + "', expected '" + probe.want[key] + "'");
                    }
                    const bad = Object.keys(probe.missing);
                    probe.expect(bad.length === 0, bad.length + " interactive controls lack a meaningful Accessible.name: "
                        + bad.map(k => k + " (" + probe.missing[k] + ")").join("; "));
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
                        throw new Error("accessible-name guard: " + probe.failures.join("; "));
                    }
                    console.warn("ACCESSIBLE_NAMES_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn every_interactive_connections_control_has_an_accessible_name() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    // As an assistive technology would; Controls fill in their accessible
    // role and name only then.
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
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
            let qml = QML.replace("    // @a11y-guard\n", GUARD);
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
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
    assert!(
        bad.is_empty(),
        "accessible-name guard failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
