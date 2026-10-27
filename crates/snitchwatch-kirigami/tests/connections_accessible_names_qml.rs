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
    property var missing: ({})
    property int checked: 0
    property int phase: 0
    property int stalls: 0
    readonly property int maxStalls: 200
    property var geo: ({})
    readonly property string rec: "snitchwatch-default-app"
    readonly property var actions: page.connectionActions
    readonly property var menu: page.connectionActions.menu
    readonly property var list: page.connectionList
    // Roles a user operates; each needs a name a screen reader can say.
    readonly property var interactive: [Accessible.Button, Accessible.CheckBox,
        Accessible.RadioButton, Accessible.ComboBox, Accessible.EditableText,
        Accessible.Slider, Accessible.SpinBox, Accessible.MenuItem,
        Accessible.ButtonMenu, Accessible.ButtonDropDown, Accessible.Link]
    // (Not PageTab: a Kirigami page reports it, and it is a container.)

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
    function find(root, pred) {
        if (!root) return null;
        if (pred(root)) return root;
        const kids = root.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.find(kids[i], pred);
            if (f) return f;
        }
        return null;
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
    function describe(item) {
        const text = item.text !== undefined ? String(item.text) : "";
        return String(item).split("(")[0] + " role " + item.Accessible.role
            + (item.objectName ? " '" + item.objectName + "'" : "")
            + (text ? " text '" + text.slice(0, 40) + "'" : "");
    }
    // Every shown interactive item under `item` must have a name.
    function walk(item, where) {
        if (!item || !item.visible) return;
        const role = item.Accessible ? item.Accessible.role : 0;
        // Read-only text (a message, a non-editable combo box's display) is
        // read out as content, not operated: it is not a control.
        const staticText = role === Accessible.EditableText && item.readOnly === true;
        if (probe.interactive.indexOf(role) >= 0 && !staticText && item.width > 0
                && item.height > 0 && !item.Accessible.ignored) {
            probe.checked++;
            probe.seen["role " + role] = (probe.seen["role " + role] || 0) + 1;
            const name = String(item.Accessible.name || "").trim();
            probe.names[name] = true;
            const why = probe.unnamed(name, item, role);
            if (why) probe.missing[probe.describe(item) + ": " + why] = where;
            // Its parts (a combo box's display field) are named by it.
            return;
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) probe.walk(kids[i], where);
    }
    // Controls checked per sweep, by role.
    property var seen: ({})
    property var names: ({})
    // Words that name a kind of control, not this one.
    readonly property var generic: ["button", "push button", "combobox", "combo box", "switch",
        "checkbox", "check box", "menuitem", "menu item", "item", "textfield", "text field",
        "text", "edit", "toolbutton", "tool button", "control"]
    // Why `name` does not name the control, or "" when it does.
    function unnamed(name, item, role) {
        if (name.length < 2) return "no name";
        if (probe.generic.indexOf(name.toLowerCase()) >= 0) return "a generic name";
        if (role === Accessible.ComboBox) {
            const value = String(item.displayText || "").trim();
            if (name === value) return "only its value, no label";
            if (/:\s*$/.test(name)) return "a label with an empty value";
        }
        return "";
    }
    function sweep(where) {
        const before = probe.checked;
        probe.walk(probe.contentItem, where);
        probe.seen[where] = probe.checked - before;
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
                    // A choice and a model change: names follow the values.
                    const scope = probe.find(page, it => String(it.Accessible && it.Accessible.name).indexOf("Scope:") === 0);
                    scope.currentIndex = 1;
                    probe.send({ action: "insertConnectionRows", rows: [probe.row("n", "new.example.org", null, null)] });
                } else if (probe.phase === 9) {
                    probe.names = {};
                    probe.sweep("after a change");
                    probe.expect(probe.names["Scope: Any host on this domain"] === true,
                                 "the Scope name did not follow its value: "
                                 + JSON.stringify(Object.keys(probe.names).filter(n => n.indexOf("Scope") === 0)));
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
