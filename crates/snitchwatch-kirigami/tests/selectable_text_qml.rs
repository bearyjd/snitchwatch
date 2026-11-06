//! Selectable text, pilot (plan `2026-10-30-selectable-text.md`): the About
//! page's text (class A) and the Profiles row (class C), with REAL mouse and
//! key events through QtTest.
//!   * dragging over a value selects it, and Ctrl+C puts it on the clipboard;
//!   * the text is read-only, reports the StaticText role and is no Tab stop;
//!   * the right-click menu's Copy copies the selection;
//!   * right-clicking a Profiles row offers Copy, which puts the row's plain
//!     text on the clipboard and opens nothing; a left click still opens it;
//!   * Space auto-repeat does not press the menu's Copy;
//!   * the Tab itinerary over both pages is exactly the pre-existing one.
//!
//! The clipboard is read back with `TextEdit.paste()`. Run headless with
//! `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::profiles_model as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_text_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_TEXT_PROBE_DONE";

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
    readonly property string doneMarker: "SELECTABLE_TEXT_PROBE_DONE"
    // @a11y-guard
    // @list-keys

    Item { id: start; width: 1; height: 1; activeFocusOnTab: true }
    AboutPage {
        id: about
        x: 0; y: 10; width: 600; height: 800
        flatpakInfoPath: "/nonexistent/flatpak-info"
        daemonConnected: true
        daemonVersion: "1.8.0"
    }
    ProfilesModel { id: profiles }
    ProfilesPage {
        id: page
        x: 700; y: 10; width: 600; height: 800
        model: profiles
    }
    TextEdit { id: sink; visible: false }
    TextField { id: field; x: 700; y: 850; width: 200; activeFocusOnTab: false; Accessible.name: "Test field"; text: "FIELDTEXT" }
    function copyButton() { return probe.find(about, it => it.objectName === "aboutCopyButton"); }

    function clip() { sink.text = ""; sink.paste(); return sink.text; }
    function setClip(s) { sink.text = s; sink.selectAll(); sink.copy(); }
    function value(key) { return probe.find(about, it => it.objectName === "aboutValue_" + key); }
    function label(key) { return probe.find(about, it => it.objectName === "aboutLabel_" + key); }
    function list() { return probe.find(page, it => it.objectName === "" && it.count !== undefined && it.itemAtIndex !== undefined); }
    function row(i) { const l = probe.list(); return l ? l.itemAtIndex(i) : null; }
    function handlerOf(item) {
        for (let i = 0; i < item.data.length; ++i) if (item.data[i].objectName === "rowCopyMenu") return item.data[i];
        return null;
    }
    function ready() {
        const v = probe.value("gui_version");
        if (!v || !probe.stable("v", v)) return "the About value";
        const l = probe.list();
        if (!l || l.count !== 2) return "two profile rows";
        for (let i = 0; i < 2; ++i) if (!probe.stable("row" + i, probe.row(i))) return "row " + i;
        return "";
    }
    function menuItem(menu, name) {
        for (let i = 0; i < menu.count; ++i) if (menu.itemAt(i).objectName === name) return menu.itemAt(i);
        return null;
    }

    Component.onCompleted: {
        probe.step("activate and send", null, function () {
            probe.requestActivate();
            profiles.applyServerMessageJson(JSON.stringify({ action: "setProfiles", storage: { persistent: true },
                appliesRules: true, profiles: [
                    { id: "home", name: "Home", networkMatchers: [], active: true, rules: [] },
                    { id: "work", name: "Work", networkMatchers: ["Office*"], active: false, rules: [] }] }));
        });
        probe.step("static text, no Tab stop", probe.ready, function () {
            for (const item of [probe.value("gui_version"), probe.label("gui_version")]) {
                probe.expect(item !== null, "an About text item is missing");
                if (!item) continue;
                probe.expect(item.readOnly === true && item.selectByMouse === true, "not read-only and mouse-selectable: " + item);
                probe.expect(item.Accessible.role === Accessible.StaticText, "role " + item.Accessible.role + ", not StaticText");
                probe.expect(item.activeFocusOnTab === false, "the text is a Tab stop");
            }
            probe.sweep("pilot");
            probe.noMissingNames();
        });
        probe.step("Tab itinerary is the existing one", null, function () {
            // (The new-profile field sits in the page's title row, not drawn here.)
            const names = probe.itinerary(start, 12).map(s => s.name);
            probe.expect(JSON.stringify(names) === JSON.stringify(["Copy version info", "Home (active)", "Work"]),
                         "Tab visits " + JSON.stringify(names));
        });
        probe.step("a click never takes focus", null, function () {
            const btn = probe.copyButton();
            const v = probe.value("gui_version");
            btn.forceActiveFocus();
            const before = probe.focused();
            probe.expect(before === btn, "setup: focus is on " + probe.nameOf(before));
            tc.mouseClick(v, 5, v.height / 2);
            probe.expect(probe.focused() === before, "a click moved focus to " + probe.nameOf(probe.focused()));
            probe.expect(v.activeFocus === false, "the text has active focus");
            probe.key(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Home (active)", "Tab after a click went to " + probe.nameOf(probe.focused()));
            probe.key(Qt.Key_Backtab);
            probe.expect(probe.focused() === btn, "Shift+Tab went to " + probe.nameOf(probe.focused()));
            tc.mouseDrag(v, 2, v.height / 2, v.contentWidth + 4, 0);
            probe.expect(probe.focused() === btn, "a drag moved focus to " + probe.nameOf(probe.focused()));
            probe.expect(v.selectedText === v.text, "no selection without focus: '" + v.selectedText + "'");
            const f = probe.value("flatpak");
            tc.mouseDoubleClickSequence(f, 5, f.height / 2);
            probe.expect(f.selectedText.length > 0, "a double click selected nothing");
            probe.expect(probe.focused() === btn, "a double click moved focus to " + probe.nameOf(probe.focused()));
        });
        probe.step("arrow keys still reach the page", null, function () {
            const v = probe.value("gui_version");
            probe.row(0).forceActiveFocus();
            tc.mouseClick(v, 5, v.height / 2);
            probe.key(Qt.Key_Down);
            probe.expect(probe.nameOf(probe.focused()) === "Work", "Down after a click went to " + probe.nameOf(probe.focused()));
        });
        probe.step("drag selects, Ctrl+C copies", null, function () {
            const v = probe.value("gui_version");
            probe.setClip("SENTINEL");
            tc.mouseDrag(v, 2, v.height / 2, v.contentWidth + 4, 0);
            probe.expect(v.selectedText.length > 0, "dragging selected nothing");
            probe.expect(v.selectedText === v.text, "selected '" + v.selectedText + "' of '" + v.text + "'");
            tc.keyClick(Qt.Key_C, Qt.ControlModifier);
            probe.expect(probe.clip() === v.text, "Ctrl+C put '" + probe.clip() + "' on the clipboard, not '" + v.text + "'");
        });
        probe.step("Ctrl+C in a field copies the field", null, function () {
            const v = probe.value("gui_version");
            probe.expect(v.selectedText.length > 0, "setup: nothing selected");
            probe.setClip("SENTINEL");
            field.forceActiveFocus();
            field.selectAll();
            tc.keyClick(Qt.Key_C, Qt.ControlModifier);
            probe.expect(probe.clip() === "FIELDTEXT", "Ctrl+C in a field put '" + probe.clip() + "' on the clipboard");
            field.deselect();
            probe.setClip("SENTINEL");
            tc.keyClick(Qt.Key_C, Qt.ControlModifier);
            probe.expect(probe.clip() === "SENTINEL", "Ctrl+C in a field with no selection copied '" + probe.clip() + "' from a label");
        });
        probe.step("a new selection clears the old one", null, function () {
            const v = probe.value("gui_version"), d = probe.value("daemon_version");
            probe.copyButton().forceActiveFocus();
            tc.mouseDrag(v, 2, v.height / 2, v.contentWidth + 4, 0);
            tc.mouseDrag(d, 2, d.height / 2, d.contentWidth + 4, 0);
            probe.expect(v.selectedText === "" && d.selectedText === d.text, "selections: '" + v.selectedText + "' / '" + d.selectedText + "'");
            probe.setClip("SENTINEL");
            tc.keyClick(Qt.Key_C, Qt.ControlModifier);
            probe.expect(probe.clip() === d.text, "Ctrl+C copied '" + probe.clip() + "', not the latest selection");
        });
        probe.step("right-click Copy copies the selection", null, function () {
            const v = probe.value("daemon_version");
            probe.setClip("SENTINEL");
            v.deselect();
            tc.mouseClick(v, 2, v.height / 2, Qt.RightButton);
            const menu = v.contextMenu;
            probe.expect(menu !== null && menu.visible, "right-click opened no menu");
            if (!menu) return;
            const copy = probe.menuItem(menu, "selectableSelectAll");
            probe.expect(copy !== null, "no Select all item");
            tc.mouseClick(copy);
            probe.expect(v.selectedText === v.text, "Select all selected '" + v.selectedText + "'");
        });
        probe.step("then Copy", function () { return probe.value("daemon_version").contextMenu.visible ? "" : ""; }, function () {
            const v = probe.value("daemon_version");
            tc.mouseClick(v, 2, v.height / 2, Qt.RightButton);
            const copy = probe.menuItem(v.contextMenu, "selectableCopy");
            probe.expect(copy !== null && copy.enabled, "Copy is missing or disabled with a selection");
            tc.mouseClick(copy);
            probe.expect(probe.clip() === v.text, "Copy put '" + probe.clip() + "' on the clipboard, not '" + v.text + "'");
        });
        probe.step("a row: right-click Copy", null, function () {
            const r = probe.row(1);
            const h = probe.handlerOf(r);
            probe.expect(h !== null, "the Profiles row has no RowCopyMenu");
            if (!h) return;
            probe.setClip("SENTINEL");
            page.inspectId = "";
            tc.mouseClick(r, r.width / 2, r.height / 2, Qt.RightButton);
            probe.expect(h.menu.visible, "right-click opened no menu");
            probe.expect(page.inspectId === "", "right-click opened the row's sheet");
            // A held Space does not press Copy.
            const copy = probe.menuItem(h.menu, "rowCopyItem");
            probe.expect(copy !== null, "no Copy item");
            if (!copy) return;
            copy.forceActiveFocus();
            probe.stray(Qt.Key_Space);
            probe.expect(probe.clip() === "SENTINEL", "a held Space pressed Copy: '" + probe.clip() + "'");
            tc.mouseClick(copy);
            probe.expect(probe.clip() === "\u25cb\nWork\nOffice*",
                         "row Copy put '" + probe.clip() + "' on the clipboard");
        });
        probe.step("a left click still opens the row", function () { return probe.handlerOf(probe.row(1)).menu.visible ? "the menu to close" : ""; }, function () {
            const r = probe.row(1);
            tc.mouseClick(r, r.width / 2, r.height / 2, Qt.LeftButton);
            probe.expect(page.inspectId === "work", "a left click opened '" + page.inspectId + "'");
            probe.key(Qt.Key_Escape);
        });
    }
}
"#;

#[test]
fn text_selects_by_mouse_copies_and_stays_out_of_the_tab_order() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
