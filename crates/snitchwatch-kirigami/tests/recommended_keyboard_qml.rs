//! Q01 slice 5: the Recommended rules page by keyboard and screen reader, with
//! REAL key events (QtTest), never by calling handlers.
//!   * Tab visits the controls in visual order (the exact itinerary, and the
//!     way back); Up and Down step between rows.
//!   * Space on a row's switch asks for exactly that row's rule; a stray or
//!     held Space (auto-repeat), Return and Enter never ask.
//!   * Keep, Remove, "Review DNS choice", "Turn all on" and the orphan
//!     Remove ignore a stray Space; Remove asks first, with focus on Cancel.
//!   * What a key does is decided by the row that has focus, also when the
//!     list is replaced between the focus and the key.
//!   * Every shown control is named after what it acts on (the shared guard),
//!     in the confirming states too.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::curated_defaults_model as _;

mod common;
#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/recommended_keyboard_probe.qml";
const DONE_MARKER: &str = "RECOMMENDED_KEYBOARD_PROBE_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 1800
    readonly property string doneMarker: "RECOMMENDED_KEYBOARD_PROBE_DONE"
    property var sent: []
    // @a11y-guard
    // @list-keys

    Item { id: start; width: 1; height: 1; activeFocusOnTab: true }
    CuratedDefaultsModel { id: curated }
    Connections {
        target: curated
        function onCuratedChangeRequested(json) { probe.sent.push(JSON.parse(json)); }
    }
    RecommendedRulesPage {
        id: page
        anchors { top: start.bottom; left: parent.left; right: parent.right; bottom: parent.bottom }
        model: curated
    }

    function entry(id, on, status, broad) {
        const e = { id: id, program: "/usr/bin/" + id, allows: "may connect to " + id,
                    why: "Why " + id, on: on, status: status };
        if (broad) e.broad = true;
        return e;
    }
    function send(order, orphans) {
        const all = { flatpak: probe.entry("flatpak", false, "off"),
                      chronyc: probe.entry("chronyc", true, "installed"),
                      nm: probe.entry("nm", true, "editedByYou"),
                      old: probe.entry("old", true, "inFirewall"),
                      "dns-resolved": probe.entry("dns-resolved", false, "off", true) };
        curated.applyServerMessageJson(JSON.stringify({ action: "setCuratedDefaults",
            entries: order.map(id => all[id]), storage: { persistent: true },
            orphans: orphans === undefined ? [{ id: "gone", enabled: true }] : orphans }));
    }
    readonly property var natural: ["flatpak", "chronyc", "nm", "old", "dns-resolved"]
    function rowAt(i) { return page.entriesList.itemAtIndex(i); }
    function rowsReady() {
        if (page.entriesList.count !== probe.natural.length) return "five rows";
        for (let i = 0; i < probe.natural.length; ++i) {
            if (!probe.stable("row" + i, probe.rowAt(i))) return "row " + i;
        }
        const footer = page.entriesList.footerItem;
        return probe.stable("footer", footer) && footer.visible ? "" : "the orphans section";
    }
    function control(name) {
        return probe.find(page, it => it.Accessible && it.visible && String(it.Accessible.name) === name);
    }
    // The program of the row holding `item`, or "" outside a row.
    function rowOf(item) {
        for (let it = item; it; it = it.parent) if (it.entryId !== undefined) return it.entryId;
        return "";
    }
    function sentIds() { return probe.sent.map(m => (m.ids || []).join(",") + ":" + m.on); }
    function bin(id) { return "/usr/bin/" + id; }

    Component.onCompleted: {
        probe.step("activate and send", null, function () {
            probe.requestActivate();
            curated.applyServerMessageJson(JSON.stringify({ action: "authenticated",
                capabilities: ["curatedBroadOptInV1"] }));
            probe.send(probe.natural);
        });
        probe.step("names and Tab order", probe.rowsReady, function () {
            probe.sweep("page");
            probe.checkItinerary("page", start, [
                "Turn all on", "Turn all off",
                probe.bin("flatpak"), "Recommended rule: " + probe.bin("flatpak"),
                probe.bin("chronyc"), "Recommended rule: " + probe.bin("chronyc"),
                probe.bin("nm"), "Recommended rule: " + probe.bin("nm"),
                "Remove rule for " + probe.bin("nm"),
                probe.bin("old"), "Recommended rule: " + probe.bin("old"),
                "Keep rule for " + probe.bin("old"),
                probe.bin("dns-resolved"), "Recommended rule: " + probe.bin("dns-resolved"),
                "Review DNS choice for " + probe.bin("dns-resolved"),
                "Remove old rule snitchwatch-default-gone"]);
        });
        probe.step("arrows step between rows", null, function () {
            probe.rowAt(0).forceActiveFocus();
            const want = ["chronyc", "nm", "old", "dns-resolved", "dns-resolved"];
            for (const id of want) {
                probe.key(Qt.Key_Down);
                probe.expect(probe.nameOf(probe.focused()) === probe.bin(id),
                             "Down went to " + probe.nameOf(probe.focused()) + ", not " + probe.bin(id));
            }
            for (const id of ["old", "nm", "chronyc", "flatpak", "flatpak"]) {
                probe.key(Qt.Key_Up);
                probe.expect(probe.nameOf(probe.focused()) === probe.bin(id),
                             "Up went to " + probe.nameOf(probe.focused()) + ", not " + probe.bin(id));
            }
            // From a control inside a row, too.
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Recommended rule: " + probe.bin("flatpak"),
                         "Tab from the row went to " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Down);
            probe.expect(probe.nameOf(probe.focused()) === probe.bin("chronyc"),
                         "Down from a row's switch went to " + probe.nameOf(probe.focused()));
        });
        probe.step("Space on a switch asks for its own row only", null, function () {
            probe.sent = [];
            probe.mustTab(start, "Recommended rule: " + probe.bin("flatpak"), 30);
            probe.key(Qt.Key_Space);
            probe.expect(JSON.stringify(probe.sentIds()) === '["flatpak:true"]', "Space on flatpak: " + JSON.stringify(probe.sentIds()));
            // A key held from before focus arrived, Return and Enter: no ask.
            probe.stray(Qt.Key_Space);
            probe.key(Qt.Key_Return);
            probe.key(Qt.Key_Enter);
            probe.expect(probe.sent.length === 1, "stray or Return keys asked: " + JSON.stringify(probe.sentIds()));
            // Pressed here and held: one ask, not one per repeat.
            probe.hold(Qt.Key_Space);
            probe.expect(probe.sent.length === 2 && probe.sentIds()[1] === "flatpak:true",
                         "a held Space asked " + (probe.sent.length - 1) + " times: " + JSON.stringify(probe.sentIds()));
            // Another row, reached by arrows: that row's rule.
            probe.rowAt(2).forceActiveFocus();
            probe.key(Qt.Key_Down);
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Recommended rule: " + probe.bin("old"),
                         "arrows and Tab reached " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Space);
            probe.expect(probe.sentIds()[2] === "old:false", "Space on old: " + JSON.stringify(probe.sentIds()));
        });
        probe.step("a stray Space presses no button", null, function () {
            probe.sent = [];
            for (const [name, ask] of [["Turn all on", true], ["Turn all off", true],
                                       ["Keep rule for " + probe.bin("old"), true]]) {
                probe.mustTab(start, name, 30);
                probe.stray(Qt.Key_Space);
                probe.expect(probe.sent.length === 0, "a stray Space pressed '" + name + "': " + JSON.stringify(probe.sentIds()));
                probe.key(Qt.Key_Space);
                probe.expect(probe.sent.length === 1, "Space did not press '" + name + "'");
                probe.sent = [];
            }
            const review = "Review DNS choice for " + probe.bin("dns-resolved");
            probe.mustTab(start, review, 30);
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0 && !curated.consentOpen, "a stray Space opened the DNS choice");
        });
        probe.step("Remove asks first; focus on Cancel", null, function () {
            probe.sent = [];
            const remove = "Remove rule for " + probe.bin("nm");
            probe.mustTab(start, remove, 30);
            probe.stray(Qt.Key_Space);
            probe.expect(!probe.control("Cancel removing rule for " + probe.bin("nm")), "a stray Space started the removal");
            probe.key(Qt.Key_Space);
        });
        probe.step("confirming", function () {
            return probe.control("Cancel removing rule for " + probe.bin("nm")) ? "" : "the confirmation";
        }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Cancel removing rule for " + probe.bin("nm"),
                         "focus after Remove is on " + probe.nameOf(probe.focused()));
            probe.expect(probe.sent.length === 0, "Remove sent before the confirmation");
            probe.sweep("removal confirming");
            probe.key(Qt.Key_Backtab);
            probe.expect(probe.nameOf(probe.focused()) === "Confirm removing rule for " + probe.bin("nm"),
                         "Shift+Tab from Cancel went to " + probe.nameOf(probe.focused()));
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space removed a rule");
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && /"nm"/.test(JSON.stringify(probe.sent[0])), "Remove: " + JSON.stringify(probe.sent));
        });
        probe.step("orphan Remove", null, function () {
            probe.sent = [];
            const remove = "Remove old rule snitchwatch-default-gone";
            probe.mustTab(start, remove, 40);
            probe.stray(Qt.Key_Space);
            probe.expect(!probe.control("Cancel removing old rule snitchwatch-default-gone"), "a stray Space started the orphan removal");
            probe.key(Qt.Key_Space);
        });
        probe.step("orphan confirming", function () {
            return probe.control("Cancel removing old rule snitchwatch-default-gone") ? "" : "the orphan confirmation";
        }, function () {
            probe.expect(probe.nameOf(probe.focused()) === "Cancel removing old rule snitchwatch-default-gone",
                         "focus after the orphan Remove is on " + probe.nameOf(probe.focused()));
            probe.sweep("orphan confirming");
            probe.key(Qt.Key_Backtab);
            probe.stray(Qt.Key_Space);
            probe.expect(probe.sent.length === 0, "a stray Space removed an orphan");
            probe.key(Qt.Key_Space);
            probe.expect(probe.sent.length === 1 && probe.sent[0].id === "gone", "orphan Remove: " + JSON.stringify(probe.sent));
        });
        probe.step("the list is replaced between focus and key", null, function () {
            probe.sent = [];
            probe.mustTab(start, "Recommended rule: " + probe.bin("chronyc"), 30);
            probe.send(["dns-resolved", "old", "nm", "chronyc", "flatpak"]);
        });
        probe.step("key after the reorder", probe.rowsReady, function () {
            const shown = probe.rowOf(probe.focused());
            probe.key(Qt.Key_Space);
            // Nothing, or the rule of the row that now has focus: never the
            // row that had it before the list was replaced.
            const asked = probe.sentIds();
            probe.expect(asked.length === 0 || (shown !== "" && JSON.stringify(asked) === JSON.stringify([shown + ":" + (shown === "old" || shown === "nm" || shown === "chronyc" ? "false" : "true")])),
                         "after a reorder Space asked " + JSON.stringify(asked) + " with focus on '" + shown + "'");
            probe.mustTab(start, "Recommended rule: " + probe.bin("chronyc"), 30);
            probe.sent = [];
            tc.keyPress(Qt.Key_Space);
            probe.send(probe.natural);
        });
        probe.step("release after the reorder", probe.rowsReady, function () {
            const shown = probe.rowOf(probe.focused());
            tc.keyRelease(Qt.Key_Space);
            const asked = probe.sentIds();
            probe.expect(asked.length === 0 || (shown !== "" && asked.length === 1 && asked[0].split(":")[0] === shown),
                         "a release after a reorder asked " + JSON.stringify(asked) + " with focus on '" + shown + "'");
        });
        probe.step("names after the changes", null, function () {
            probe.names = {};
            probe.sweep("after the changes");
            probe.sawNames(["Turn all on", "Turn all off", "Recommended rule: " + probe.bin("flatpak"),
                "Keep rule for " + probe.bin("old"), "Remove rule for " + probe.bin("nm"),
                "Review DNS choice for " + probe.bin("dns-resolved"),
                "Remove old rule snitchwatch-default-gone"]);
            probe.expect(probe.checked > 15, "the guard saw too few controls: " + JSON.stringify(probe.seen));
            probe.noMissingNames();
        });
    }
}
"#;

#[test]
fn the_recommended_page_works_by_keyboard_and_names_every_control() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
