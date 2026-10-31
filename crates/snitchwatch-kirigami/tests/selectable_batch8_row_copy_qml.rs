//! Selectable text, batch 8: a Connections row's right-click "Copy row text"
//! (an item of the row's existing actions menu, `ConnectionActions.qml`)
//! carries EVERY text the row shows, full length (the host line is elided on
//! screen), checked on real `ConnectionRow` delegates of a real
//! `ConnectionsPage`: pending, held back, repeated, decided, unverified leaves;
//! collapsed and expanded program headers, a domain header, a header with
//! nothing pending. A real right-click opens the menu and the item's text lands
//! on the native clipboard; the rule button's menu does not offer it.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_batch8_row_copy_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_BATCH8_ROW_COPY_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1400
    height: 900
    readonly property string doneMarker: "SELECTABLE_BATCH8_ROW_COPY_DONE"
    // @a11y-guard
    // @list-keys

    QtObject {
        id: feedStub
        property bool ok: true
        function appBoundRulesFor(rowId) { return true; }
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) { return true; }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }
    TextEdit { id: sink; visible: false }

    readonly property var actions: page.connectionActions
    readonly property var list: page.connectionList
    readonly property real t0: Date.now() - 60000
    property int ran: 0
    readonly property int expectedSteps: 7

    function clip() { sink.text = ""; sink.paste(); return sink.text; }
    function setClip(s) { sink.text = s; sink.selectAll(); sink.copy(); }
    function row(id, process, path, host, action, at) {
        return { id: id, process: process, processPath: path, dstHost: host,
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: at,
                 matchedRule: action === null ? null : "rule-" + process };
    }
    function delegateFor(rowId) {
        const at = model.visibleRowIndex(rowId);
        if (at < 0) return null;
        probe.list.positionViewAtIndex(at, ListView.Contain);
        probe.list.forceLayout();
        for (let i = 0; i < probe.list.count; i++) {
            const d = probe.list.itemAtIndex(i);
            if (d && d.rowId === rowId && !d.isGroupHeader) return d;
        }
        return null;
    }
    function headerFor(level, key) {
        for (let i = 0; i < probe.list.count; i++) {
            const d = probe.list.itemAtIndex(i);
            if (d && d.isGroupHeader && d.groupLevel === level && d.groupKey === key) return d;
        }
        return null;
    }
    // Every text the row shows: shown text items, outside buttons (their
    // text is operated, not read; tooltips are popups, not children).
    function shownTexts(item, out) {
        if (!item || !item.visible) return out;
        if (item.checkable !== undefined && item.pressed !== undefined) return out;
        if (item.elide !== undefined && item.textFormat !== undefined && typeof item.text === "string"
                && item.text !== "")
            out.push(item.text);
        for (let i = 0; i < item.children.length; i++) probe.shownTexts(item.children[i], out);
        return out;
    }
    // The copy as the menu captures it when it opens on `d`.
    function copyOf(d) { return probe.actions.snapshot(d).rowText; }
    // The copy holds every shown text, full length (the path tail beside an
    // unverified label stands for the full path, which the copy holds).
    function expectHoldsShown(d, what, atLeast) {
        const text = probe.copyOf(d);
        const shown = probe.shownTexts(d, []);
        probe.expect(shown.length >= atLeast, what + ": only " + shown.length + " shown texts found: "
                     + JSON.stringify(shown));
        for (const s of shown) {
            const want = s === d.programPathTail && d.programLabelUnverified ? d.programPathFull : s;
            probe.expect(text.indexOf(want) >= 0, what + ": copy lacks shown '" + want + "': " + JSON.stringify(text));
        }
        probe.expect(text !== "" && text.indexOf("\n\n") < 0 && !text.endsWith("\n"),
                     what + ": empty or blank lines: " + JSON.stringify(text));
        return text;
    }
    function inOrder(text, parts, what) {
        const at = parts.map(p => text.indexOf(p));
        probe.expect(at.every(i => i >= 0) && JSON.stringify(at) === JSON.stringify(at.slice().sort((a, b) => a - b)),
                     what + ": not in the shown order " + JSON.stringify(parts) + " -> " + JSON.stringify(at)
                     + " in " + JSON.stringify(text));
    }
    function menuItem(name) {
        for (let i = 0; i < probe.actions.menu.count; ++i) {
            const item = probe.actions.menu.itemAt(i);
            if (item && item.objectName === name) return item;
        }
        return null;
    }

    Component.onCompleted: {
        // A step limit (SW_STEPS) that cuts the run short would pass vacuously.
        probe.expect(probe.stepLimit >= probe.expectedSteps, "SW_STEPS cuts the probe short: " + probe.stepLimit);
        probe.step("flat rows", null, function () {
            probe.ran++;
            model.setGroupedMode(false);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                probe.row("ask-1", "python3", "/usr/bin/python3", "example.com", null, probe.t0),
                probe.row("ask-2", "python3", "/usr/bin/python3", "example.com", null, probe.t0 + 1000),
                probe.row("event-1", "curl", "/usr/bin/curl", "deny.example", "deny", probe.t0 + 2000),
                probe.row("event-2", "curl", "/usr/bin/curl", "deny.example", "deny", probe.t0 + 3000),
                probe.row("event-3", "curl", "/usr/bin/curl", "deny.example", "deny", probe.t0 + 4000),
                probe.row("ver-1", "claude", "/opt/claude/versions/2.1.3", "v.example", "allow", probe.t0 + 5000)] }));
            model.holdBackNoticesForTests("/usr/bin/python3", "example.com", 3, 3600);
        });
        probe.step("leaf copies hold every shown text", () => {
            const d = probe.delegateFor("ask-2");
            return d && d.height > 0 && d.retriedCount > 0 ? "" : "held-back row";
        }, function () {
            probe.ran++;
            const held = probe.delegateFor("ask-2");
            const text = probe.expectHoldsShown(held, "pending held back", 4);
            const lines = text.split("\n");
            probe.expect(lines[0] === "◐ " + held.programLabel, "first line: " + JSON.stringify(lines[0]));
            probe.expect(lines.indexOf("notice held back 3 times in the last hour") >= 0, "no held-back note: " + text);
            probe.expect(lines[lines.length - 1] === "pending", "verdict not last: " + text);
            probe.inOrder(text, [held.programLabel, "example.com:443  tcp", "notice held back", "pending"], "pending");

            const older = probe.delegateFor("ask-1");
            const olderText = probe.expectHoldsShown(older, "pending", 4);
            probe.expect(olderText.indexOf("held back") < 0, "a hidden note leaked: " + olderText);

            const repeated = probe.delegateFor("event-1");
            const repText = probe.expectHoldsShown(repeated, "repeated", 4);
            probe.expect(repeated.repeatCount === 3, "fixture: repeatCount " + repeated.repeatCount);
            probe.inOrder(repText, ["curl", "×3 · last seen ", "deny.example:443  tcp", probe.actions.verdictText(repeated)], "repeated");
            probe.expect(repText.indexOf("pending") < 0 && repText.indexOf("held back") < 0, "repeated: " + repText);

            const unverified = probe.delegateFor("ver-1");
            probe.expect(unverified.programLabelUnverified, "fixture: label is verified");
            const verText = probe.expectHoldsShown(unverified, "unverified", 5);
            probe.inOrder(verText, [unverified.programLabel + " ?", "/opt/claude/versions/2.1.3", "v.example:443  tcp"],
                          "unverified");
        });
        probe.step("a real right-click copies the row", () => {
            const d = probe.delegateFor("ask-2");
            return d && d.height > 0 ? "" : "row";
        }, function () {
            probe.ran++;
            const d = probe.delegateFor("ask-2");
            probe.setClip("BEFORE");
            tc.mouseClick(d, 2, 2, Qt.RightButton);
            probe.expect(probe.actions.menu.visible, "right-click did not open the row's actions");
            const item = probe.menuItem("copyRowText");
            probe.expect(item && item.visible && item.enabled && item.height > 0, "no shown, enabled Copy row text");
            probe.expect(item && item.text === "Copy row text", "item text: " + (item ? item.text : "none"));
            // Every pre-existing copy item is still there beside it.
            for (const n of ["copyExecutable", "copyHost", "copyIp", "copyPort"])
                probe.expect(probe.menuItem(n) !== null, "lost " + n);
            const want = probe.copyOf(d);
            if (item) item.triggered();
            probe.expect(probe.clip() === want, "clipboard '" + probe.clip() + "' != '" + want + "'");
            probe.actions.close();
            model.setGroupedMode(true);
        });
        probe.step("headers: collapsed", () => {
            if (probe.actions.menu.visible) return "menu closed";
            return probe.headerFor(0, "/usr/bin/python3") ? "" : "program header";
        }, function () {
            probe.ran++;
            const h = probe.headerFor(0, "/usr/bin/python3");
            probe.expect(!h.expanded && h.groupPending === 2, "fixture: " + h.expanded + " " + h.groupPending);
            const text = probe.expectHoldsShown(h, "collapsed header", 5);
            probe.inOrder(text, [h.programLabel, "2 connections", "2 pending", "Last hour", "Today"], "collapsed header");
            const quiet = probe.headerFor(0, "/usr/bin/curl");
            const quietText = probe.expectHoldsShown(quiet, "header, nothing pending", 4);
            probe.expect(quietText.indexOf(" pending") < 0, "nothing pending, yet: " + quietText);
            model.toggleProcessGroup("/usr/bin/python3");
        });
        probe.step("headers: expanded and domain", () => {
            const h = probe.headerFor(0, "/usr/bin/python3");
            return h && h.expanded && probe.headerFor(1, "example.com") ? "" : "expanded header";
        }, function () {
            probe.ran++;
            const h = probe.headerFor(0, "/usr/bin/python3");
            const text = probe.expectHoldsShown(h, "expanded header", 5);
            probe.inOrder(text, [h.programLabel, "2 connections", "2 pending", "Last hour"], "expanded header");
            const domain = probe.headerFor(1, "example.com");
            const domainText = probe.expectHoldsShown(domain, "domain header", 5);
            probe.inOrder(domainText, [domain.programLabel, "2 connections", "2 pending"], "domain header");
        });
        probe.step("the rule button's menu does not offer it", null, function () {
            probe.ran++;
            const h = probe.headerFor(0, "/usr/bin/python3");
            probe.actions.openRule(h, h.ruleButton);
            const item = probe.menuItem("copyRowText");
            probe.expect(item && !item.visible && item.height === 0, "Copy row text on the rule menu");
            probe.actions.close();
        });
        // Last: every step above really ran (a skipped step must not pass).
        probe.step("steps executed == " + probe.expectedSteps, null, function () {
            probe.expect(probe.ran + 1 === probe.expectedSteps,
                         "steps executed " + (probe.ran + 1) + " != " + probe.expectedSteps);
        });
    }
}
"#;

#[test]
fn connection_row_copy_has_every_shown_text() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
