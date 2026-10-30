//! Q01 slice 5: the Profiles page by keyboard and screen reader, with REAL key
//! events (QtTest), never by calling handlers.
//!   * Tab visits the new-profile field and the rows in visual order; Up and
//!     Down step between rows; Return or Space opens the row that has focus.
//!   * A stray or held Space or Return (auto-repeat) opens nothing, and
//!     creates, renames, activates, deletes or removes nothing.
//!   * The detail sheet: exact Tab itinerary, fields named after their labels,
//!     Escape closes it, deleting asks first with focus on Cancel.
//!   * What a key does is decided by the row that has focus, also when the
//!     list is replaced between the focus and the key.
//!   * Every shown control is named (the shared guard), in the sheet and in
//!     the confirming states too.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::profiles_model as _;

mod common;
#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/profiles_keyboard_probe.qml";
const DONE_MARKER: &str = "PROFILES_KEYBOARD_PROBE_DONE";

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
    readonly property string doneMarker: "PROFILES_KEYBOARD_PROBE_DONE"
    property var sent: []
    property var titleRow: null
    // @a11y-guard
    // @list-keys

    Item { id: start; width: 1; height: 1; activeFocusOnTab: true }
    // The title row (only a page stack draws a page's `titleDelegate`).
    Item { id: titleHost; anchors.top: start.bottom; width: parent.width; height: 48 }
    ProfilesModel { id: profiles }
    Connections {
        target: profiles
        function onProfileChangeRequested(json) { probe.sent.push(JSON.parse(json)); }
    }
    ProfilesPage {
        id: page
        anchors { top: titleHost.bottom; left: parent.left; right: parent.right; bottom: parent.bottom }
        model: profiles
    }

    function profile(id, name, active, rules) {
        return { id: id, name: name, networkMatchers: [], active: active, rules: rules || [] };
    }
    readonly property var homeRules: [
        { id: "r1", action: "deny", operand: "dest.host", data: "ads.example", enforcement: "rule_installed" },
        { id: "r2", action: "allow", operand: "dest.host", data: "x.example", enforcement: "rule_installed" }]
    function send(order) {
        const all = { home: probe.profile("home", "Home", true, probe.homeRules),
                      work: probe.profile("work", "Work", false),
                      cafe: probe.profile("cafe", "Cafe", false) };
        profiles.applyServerMessageJson(JSON.stringify({ action: "setProfiles", storage: { persistent: true },
            appliesRules: true, profiles: order.map(id => all[id]) }));
    }
    readonly property var natural: ["home", "work", "cafe"]
    function list() { return probe.find(page, it => it.objectName === "" && it.count !== undefined && it.itemAtIndex !== undefined); }
    function row(i) { const l = probe.list(); return l ? l.itemAtIndex(i) : null; }
    function rowsReady() {
        const l = probe.list();
        if (!l || l.count !== 3) return "three rows";
        for (let i = 0; i < 3; ++i) if (!probe.stable("row" + i, probe.row(i))) return "row " + i;
        return probe.titleRow && probe.stable("title", probe.titleRow) ? "" : "the title row";
    }
    function sheet() { return page.inspectorSheet; }
    function control(name) {
        return probe.find(probe.sheet().contentItem, it => it.Accessible && it.visible && String(it.Accessible.name) === name);
    }
    function sheetReady() { return probe.sheet().opened && probe.stable("sheet", probe.sheet().contentItem) ? "" : "the sheet"; }
    function rowOf(item) {
        for (let it = item; it; it = it.parent) if (it.profileId !== undefined) return it.profileId;
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
    // Tab around inside the open sheet (focus cannot leave a popup).
    function sheetStops() {
        const first = probe.focused();
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
            probe.send(probe.natural);
        });
        probe.step("names and Tab order", probe.rowsReady, function () {
            probe.sheet().enter = null;
            probe.sheet().exit = null;
            probe.sweep("page");
            probe.checkItinerary("page", start, ["New profile name", "Home (active)", "Work", "Cafe"]);
        });
        probe.step("arrows step between rows", null, function () {
            probe.row(0).forceActiveFocus();
            for (const name of ["Work", "Cafe", "Cafe"]) {
                probe.key(Qt.Key_Down);
                probe.expect(probe.nameOf(probe.focused()) === name, "Down went to " + probe.nameOf(probe.focused()) + ", not " + name);
            }
            for (const name of ["Work", "Home (active)", "Home (active)"]) {
                probe.key(Qt.Key_Up);
                probe.expect(probe.nameOf(probe.focused()) === name, "Up went to " + probe.nameOf(probe.focused()) + ", not " + name);
            }
        });
        probe.step("a new profile by keyboard", null, function () {
            probe.sent = [];
            probe.mustTab(start, "New profile name", 8);
            probe.typeText("Lab");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Create profile", "Tab from the field went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.key(Qt.Key_Return);
            probe.expect(probe.sent.length === 0, "a stray key created a profile: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.kinds().join() === "createProfile" && probe.sent[0].name === "Lab", "Create: " + JSON.stringify(probe.sent));
            probe.sweep("create enabled");
        });
        probe.step("a held key opens no row", null, function () {
            probe.sent = [];
            probe.row(2).forceActiveFocus();
            probe.stray(Qt.Key_Space);
            tc.keyPress(Qt.Key_Return, probe.repeat);
            tc.keyRelease(Qt.Key_Return, probe.repeat);
            probe.expect(!probe.sheet().opened && page.inspectId === "", "a held key opened '" + page.inspectId + "'");
            probe.key(Qt.Key_Up);
            probe.key(Qt.Key_Return);
        });
        probe.step("Return opens the focused row", function () { return probe.sheetReady(); }, function () {
            probe.expect(page.inspectId === "work", "Return on Work opened '" + page.inspectId + "'");
            probe.expect(probe.inSheet(probe.focused()), "focus is not in the sheet: " + probe.nameOf(probe.focused()) + " " + probe.focused() + " parent " + probe.focused().parent + " sheetContent " + probe.sheet().contentItem);
            probe.key(Qt.Key_Escape);
        });
        probe.step("Escape closes and focus returns", function () { return probe.sheet().opened || probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Work", "focus after Escape is on " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Up);
            probe.key(Qt.Key_Space);
        });
        probe.step("Home's sheet", probe.sheetReady, function () {
            probe.expect(page.inspectId === "home", "Space on Home opened '" + page.inspectId + "'");
            probe.sweep("home sheet");
            const walk = probe.sheetStops();
            const want = ["Close", "Profile name", "Network matchers",
                "Remove rule: deny dest.host = ads.example", "Remove rule: allow dest.host = x.example",
                "Add rule…", "Deactivate", "Delete profile Home"];
            probe.expect(JSON.stringify(walk.map(s => s.name)) === JSON.stringify(want),
                         "sheet Tab itinerary: " + JSON.stringify(walk.map(s => s.name)));
            const bad = probe.readingOrder(walk);
            probe.expect(bad === "", "the sheet's Tab order is not the visual order: " + bad);
            probe.sent = [];
        });
        probe.step("rename by keyboard", null, function () {
            probe.sheetTab("Profile name");
            probe.typeText("X");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Save name", "Tab from the name went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space renamed: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && probe.sent[0].action === "updateProfile" && probe.sent[0].name === "HomeX",
                         "Save name: " + JSON.stringify(probe.sent));
            probe.sweep("rename changed");
            probe.sent = [];
        });
        probe.step("matchers by keyboard", null, function () {
            probe.sheetTab("Network matchers");
            probe.typeText("Cafe*");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Save matchers", "Tab from the matchers went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space saved matchers: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && probe.sent[0].action === "updateProfile" && probe.sent[0].networkMatchers.join() === "Cafe*", "Save matchers: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("remove a rule", null, function () {
            probe.sheetTab("Remove rule: allow dest.host = x.example");
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space removed a rule: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /r2/.test(JSON.stringify(probe.sent[0])), "Remove rule: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("deactivate", null, function () {
            probe.sheetTab("Deactivate");
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space deactivated: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /deactivate/i.test(probe.sent[0].action), "Deactivate: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("activate", function () { return probe.control("Activate") ? "" : "Activate"; }, function () {
            probe.sweep("inactive profile");
            probe.sheetTab("Activate");
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space activated: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /activate/i.test(probe.sent[0].action), "Activate: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("delete asks first; focus on Cancel", function () { return probe.control("Delete profile HomeX") ? "" : "Delete"; }, function () {
            probe.sheetTab("Delete profile HomeX");
            probe.stray(Qt.Key_Space);
            probe.expect(!probe.control("Cancel deleting profile HomeX"), "a stray Space started the deletion");
            probe.key(Qt.Key_Space);
        });
        probe.step("deleting", function () { return probe.control("Cancel deleting profile HomeX") ? "" : "the confirmation"; }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Cancel deleting profile HomeX", "focus after Delete is on " + probe.nameOf(probe.focused()));
            probe.sweep("deleting");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Confirm deleting profile HomeX", "Tab from Cancel went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space deleted: " + JSON.stringify(probe.sent));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /delete/i.test(probe.sent[0].action), "Delete: " + JSON.stringify(probe.sent));
            probe.sent = [];
        });
        probe.step("the list is replaced between focus and Return", function () { return probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.send(probe.natural);
        });
        probe.step("focus a row", probe.rowsReady, function () {
            probe.row(1).forceActiveFocus();
            probe.send(["cafe", "work", "home"]);
        });
        probe.step("Return after the reorder", probe.rowsReady, function () {
            const shown = probe.rowOf(probe.focused());
            page.inspectId = "";
            probe.key(Qt.Key_Return);
            // Nothing, or the row that now has focus: never the one that had it.
            probe.expect(page.inspectId === "" || (shown !== "" && page.inspectId === shown),
                         "after a reorder Return opened '" + page.inspectId + "' with focus on '" + shown + "'");
            probe.key(Qt.Key_Escape);
        });
        probe.step("names after", function () { return probe.sheet().visible ? "the sheet to close" : ""; }, function () {
            probe.sweep("end");
            probe.sawNames(["New profile name", "Create profile", "Profile name", "Network matchers", "Save name",
                "Save matchers", "Activate", "Deactivate", "Delete profile HomeX", "Cancel deleting profile HomeX",
                "Confirm deleting profile HomeX", "Remove rule: deny dest.host = ads.example"]);
            probe.noMissingNames();
        });
    }
}
"#;

#[test]
fn the_profiles_page_works_by_keyboard_and_names_every_control() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
