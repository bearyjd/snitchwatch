//! Q01 slice 5: the Blocklists page by keyboard and screen reader, with REAL
//! key events (QtTest), never by calling handlers.
//!   * Tab visits the URL field, the leftover-rules buttons and the rows in
//!     visual order (the exact itinerary, and the way back); Up and Down step
//!     between rows; Return or Space opens the row that has focus.
//!   * A stray or held Space or Return (auto-repeat) opens nothing and
//!     subscribes, unsubscribes or removes nothing.
//!   * The detail sheet: exact Tab itinerary, Escape closes it with focus back
//!     on the row, Unsubscribe acts on the list the sheet shows.
//!   * What a key does is decided by the row that has focus, also when the
//!     list is replaced between the focus and the key.
//!   * Every shown control is named (the shared guard), in the sheet and in
//!     the confirming states too.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::blocklists_model as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/blocklists_keyboard_probe.qml";
const DONE_MARKER: &str = "BLOCKLISTS_KEYBOARD_PROBE_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 1400
    readonly property string doneMarker: "BLOCKLISTS_KEYBOARD_PROBE_DONE"
    property var sent: []
    property var titleRow: null
    // @a11y-guard
    // @list-keys

    Item { id: start; width: 1; height: 1; activeFocusOnTab: true }
    // The title row (only a page stack draws a page's `titleDelegate`).
    Item { id: titleHost; anchors.top: start.bottom; width: parent.width; height: 48 }
    BlocklistsModel { id: lists }
    BlocklistEntriesModel { id: entries }
    Connections {
        target: lists
        function onSubscriptionRequested(json) { probe.sent.push(JSON.parse(json)); }
    }
    BlocklistsPage {
        id: page
        anchors { top: titleHost.bottom; left: parent.left; right: parent.right; bottom: parent.bottom }
        model: lists
        entriesModel: entries
    }

    function blocklist(id, name) {
        return { id: id, displayName: name, url: "https://example.invalid/" + id + ".txt", entryCount: 4,
                 status: "ok", enforcement: "rule_installed" };
    }
    function send(order, leftovers) {
        const all = { l1: probe.blocklist("l1", "Ads"), l2: probe.blocklist("l2", "Trackers"),
                      l3: probe.blocklist("l3", "Malware") };
        lists.applyServerMessageJson(JSON.stringify({ action: "setBlocklists", storage: { persistent: true },
            blocklists: order.map(id => all[id]) }));
        lists.applyServerMessageJson(JSON.stringify({ action: "setBlocklistLeftovers", count: leftovers }));
    }
    readonly property var natural: ["l1", "l2", "l3"]
    function list() { return probe.find(page, it => it.count !== undefined && it.itemAtIndex !== undefined && it.headerItem !== undefined); }
    function row(i) { const l = probe.list(); return l ? l.itemAtIndex(i) : null; }
    function rowsReady() {
        const l = probe.list();
        if (!l || l.count !== 3) return "three rows";
        for (let i = 0; i < 3; ++i) if (!probe.stable("row" + i, probe.row(i))) return "row " + i;
        return probe.titleRow && probe.stable("title", probe.titleRow) ? "" : "the title row";
    }
    function sheet() { return page.inspectorSheet; }
    function control(name) {
        return probe.find(probe.sheet().contentItem, it => it.Accessible && it.visible && String(it.Accessible.name) === name)
            || probe.find(page, it => it.Accessible && it.visible && String(it.Accessible.name) === name);
    }
    function sheetReady() { return probe.sheet() && probe.sheet().opened && probe.stable("sheet", probe.sheet().contentItem) ? "" : "the sheet"; }
    function rowOf(item) {
        for (let it = item; it; it = it.parent) if (it.listId !== undefined) return it.listId;
        return "";
    }
    function kinds() { return probe.sent.map(m => m.action); }
    function inSheet(item) {
        for (let it = item; it; it = it.parent) if (it === probe.sheet().contentItem || it === probe.sheet()) return true;
        return false;
    }
    function sheetTab(name) {
        for (let i = 0; i < 14; ++i) {
            probe.key(Qt.Key_Tab);
            if (probe.nameOf(probe.focused()) === name) return;
        }
        probe.expect(false, "no Tab to '" + name + "' in the sheet (at " + probe.nameOf(probe.focused()) + ")");
    }
    function sheetStops() {
        const out = [];
        for (let i = 0; i < 30; ++i) {
            probe.key(Qt.Key_Tab);
            const f = probe.focused();
            if (!f || out.some(o => o.name === probe.nameOf(f))) break;
            const p = f.mapToItem(null, 0, 0);
            out.push({ name: probe.nameOf(f), x: p.x, y: p.y });
        }
        return out;
    }

    Component.onCompleted: {
        probe.step("activate and send", null, function () {
            probe.requestActivate();
            probe.titleRow = page.titleDelegate.createObject(titleHost, { width: titleHost.width });
            probe.send(probe.natural, 2);
        });
        probe.step("names and Tab order", probe.rowsReady, function () {
            probe.sweep("page");
            probe.checkItinerary("page", start, ["Blocklist URL to subscribe",
                "Remove leftover blocklist rules", "Ads", "Trackers", "Malware"]);
        });
        probe.step("arrows step between rows", null, function () {
            probe.row(0).forceActiveFocus();
            for (const name of ["Trackers", "Malware", "Malware"]) {
                probe.key(Qt.Key_Down);
                probe.expect(probe.nameOf(probe.focused()) === name, "Down went to " + probe.nameOf(probe.focused()) + ", not " + name);
            }
            for (const name of ["Trackers", "Ads", "Ads"]) {
                probe.key(Qt.Key_Up);
                probe.expect(probe.nameOf(probe.focused()) === name, "Up went to " + probe.nameOf(probe.focused()) + ", not " + name);
            }
        });
        probe.step("subscribe by keyboard", null, function () {
            probe.sent = [];
            probe.mustTab(start, "Blocklist URL to subscribe", 12);
            probe.typeText("https://x.invalid/a.txt");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Subscribe to blocklist", "Tab from the field went to " + probe.nameOf(probe.focused()));
            probe.sweep("subscribe enabled");
            probe.stray(Qt.Key_Space);
            probe.key(Qt.Key_Return);
            probe.expect(probe.sent.length === 0, "a stray key subscribed: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && JSON.stringify(probe.sent[0]).indexOf("https://x.invalid/a.txt") >= 0, "Subscribe: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("leftover rules ask first; focus on Cancel", null, function () {
            probe.mustTab(start, "Remove leftover blocklist rules", 12);
            probe.stray(Qt.Key_Space);
            probe.expect(!probe.control("Cancel removing leftover blocklist rules"), "a stray Space started the removal");
            probe.key(Qt.Key_Space);
        });
        probe.step("leftovers confirming", function () { return probe.control("Cancel removing leftover blocklist rules") ? "" : "the confirmation"; }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Cancel removing leftover blocklist rules", "focus after Remove is on " + probe.nameOf(probe.focused()));
            probe.sweep("leftovers confirming");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Confirm removing leftover blocklist rules", "Tab from Cancel went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space removed the rules: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /leftover/i.test(probe.sent[0].action), "Remove leftovers: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("a held key opens no row", null, function () {
            probe.row(2).forceActiveFocus();
            probe.stray(Qt.Key_Space);
            tc.keyPress(Qt.Key_Return, probe.repeat);
            tc.keyRelease(Qt.Key_Return, probe.repeat);
            probe.expect(!probe.sheet().opened && page.inspectId === "", "a held key opened '" + page.inspectId + "'");
            probe.key(Qt.Key_Up);
            probe.key(Qt.Key_Return);
        });
        probe.step("Return opens the focused row", probe.sheetReady, function () {
            probe.expect(page.inspectId === "l2", "Return on Trackers opened '" + page.inspectId + "'");
            probe.expect(probe.inSheet(probe.focused()), "focus is not in the sheet: " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Escape);
        });
        probe.step("Escape closes and focus returns", function () { return probe.sheet().opened || probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Trackers", "focus after Escape is on " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Up);
            probe.key(Qt.Key_Space);
        });
        probe.step("Ads' sheet", probe.sheetReady, function () {
            probe.expect(page.inspectId === "l1", "Space on Ads opened '" + page.inspectId + "'");
            const asked = probe.sent.filter(m => m.action === "requestBlocklistEntries").pop();
            probe.expect(!!asked, "the sheet asked for no hosts");
            entries.applyServerMessageJson(JSON.stringify({ action: "setBlocklistEntries", subscriptionId: "l1",
                entries: [{ host: "a.example" }, { host: "b.example" }], offset: 0, total: 4,
                requestId: asked ? asked.requestId : "" }));
        });
        probe.step("the sheet's Tab order", function () { return entries.count === 2 && probe.control("Show more hosts") ? "" : "the hosts"; }, function () {
            probe.sweep("ads sheet");
            const walk = probe.sheetStops();
            const want = ["Close", "Show more hosts", "Unsubscribe from Ads"];
            probe.expect(JSON.stringify(walk.map(s => s.name)) === JSON.stringify(want),
                         "sheet Tab itinerary: " + JSON.stringify(walk.map(s => s.name)));
            const bad = probe.readingOrder(walk);
            probe.expect(bad === "", "the sheet's Tab order is not the visual order: " + bad);
            probe.sent = [];
        });
        probe.step("Unsubscribe", null, function () {
            probe.sheetTab("Unsubscribe from Ads");
            probe.stray(Qt.Key_Space);
            probe.key(Qt.Key_Return);
            probe.expect(probe.sent.length === 0, "a stray key unsubscribed: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && probe.sent[0].action === "unsubscribeBlocklist" && probe.sent[0].id === "l1",
                         "Unsubscribe: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("the list is replaced between focus and Return", function () { return probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.send(probe.natural, 0);
        });
        probe.step("focus a row", probe.rowsReady, function () {
            probe.row(1).forceActiveFocus();
            probe.send(["l3", "l2", "l1"], 0);
        });
        probe.step("Return after the reorder", probe.rowsReady, function () {
            const shown = probe.rowOf(probe.focused());
            page.inspectId = "";
            probe.key(Qt.Key_Return);
            probe.expect(page.inspectId === "" || (shown !== "" && page.inspectId === shown),
                         "after a reorder Return opened '" + page.inspectId + "' with focus on '" + shown + "'");
            probe.key(Qt.Key_Escape);
        });
        // The leftover-rules warning stays in view while the list scrolls, and
        // a narrow window wraps its text with every button inside the viewport.
        probe.step("many rows, scrolled to the end, narrow", probe.rowsReady, function () {
            probe.width = 420;
            const all = [];
            for (let i = 0; i < 40; ++i) all.push(probe.blocklist("m" + i, "Many " + i));
            lists.applyServerMessageJson(JSON.stringify({ action: "setBlocklists", storage: { persistent: true },
                blocklists: all }));
            lists.applyServerMessageJson(JSON.stringify({ action: "setBlocklistLeftovers", count: 2 }));
        });
        probe.step("the warning is in view", function () {
            const l = probe.list();
            return l && l.count === 40 && probe.control("Remove leftover blocklist rules") ? "" : "the long list and the warning";
        }, function () {
            probe.list().positionViewAtEnd();
        });
        probe.step("check the viewport", function () { return probe.stable("remove", probe.control("Remove leftover blocklist rules")) ? "" : "the warning to settle"; }, function () {
            probe.expect(probe.list().contentY > 100, "the list did not scroll: contentY " + probe.list().contentY);
            const inView = function (item) {
                if (!item) return "missing";
                for (let it = item; it; it = it.parent) if (it === probe.list().contentItem) return "inside the scrolling content";
                const p = item.mapToItem(null, 0, 0);
                const ok = p.x >= 0 && p.y >= 0 && p.x + item.width <= probe.width && p.y + item.height <= probe.height;
                return ok ? "" : "outside the window: " + p.x + "," + p.y + " " + item.width + "x" + item.height + " in " + probe.width + "x" + probe.height;
            };
            const remove = probe.control("Remove leftover blocklist rules");
            probe.expect(inView(remove) === "", "Remove leftover rules, list scrolled to the end: " + inView(remove));
            const text = probe.find(page, it => it.objectName === "leftoverText");
            probe.expect(!!text && inView(text) === "", "leftover warning text, list scrolled to the end: " + (text ? inView(text) : "missing"));
            probe.expect(!!text && text.lineCount > 1 && text.width <= probe.width, "the warning does not wrap in a narrow window: lines "
                         + (text ? text.lineCount + ", width " + text.width : "n/a"));
            probe.mustTab(start, "Remove leftover blocklist rules", 8);
            probe.key(Qt.Key_Space);
        });
        probe.step("confirming buttons are in view", function () { return probe.control("Cancel removing leftover blocklist rules") ? "" : "the confirmation"; }, function () {
            for (const name of ["Cancel removing leftover blocklist rules", "Confirm removing leftover blocklist rules"]) {
                const b = probe.control(name);
                const p = b ? b.mapToItem(null, 0, 0) : null;
                probe.expect(!!p && p.x >= 0 && p.x + b.width <= probe.width && p.y >= 0 && p.y + b.height <= probe.height,
                             "'" + name + "' is outside the narrow window: " + (p ? p.x + "," + p.y + " " + b.width + "x" + b.height : "missing"));
            }
            probe.key(Qt.Key_Escape);
        });
        probe.step("names after", function () { return probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.sweep("end");
            probe.sawNames(["Blocklist URL to subscribe", "Subscribe to blocklist", "Remove leftover blocklist rules",
                "Cancel removing leftover blocklist rules", "Confirm removing leftover blocklist rules",
                "Show more hosts", "Unsubscribe from Ads"]);
            probe.noMissingNames();
        });
    }
}
"#;

#[test]
fn the_blocklists_page_works_by_keyboard_and_names_every_control() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
