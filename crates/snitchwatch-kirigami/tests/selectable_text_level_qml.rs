//! Selectable text, batch 1: `SelectableText.level` stands in for
//! `Kirigami.Heading.level` (plan `2026-10-30-selectable-text.md`, "Headings").
//!   * a leveled text has the point size of a Kirigami.Heading of that level
//!     and reports the Heading role, so a screen reader still announces a
//!     heading;
//!   * level 0 (the default) is exactly the old SelectableText: same font as a
//!     plain TextEdit, StaticText role;
//!   * a leveled text is still no Tab stop and never takes focus.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_text_level_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_TEXT_LEVEL_PROBE_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls
import QtTest
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 600
    height: 600
    readonly property string doneMarker: "SELECTABLE_TEXT_LEVEL_PROBE_DONE"
    // @a11y-guard
    // @list-keys

    Item { id: start; width: 1; height: 1; activeFocusOnTab: true }
    Column {
        Repeater {
            model: [1, 2, 3, 4, 5]
            Item {
                required property int modelData
                width: 300; height: 40
                Kirigami.Heading { objectName: "heading" + parent.modelData; level: parent.modelData; text: "Heading" }
                SelectableText { objectName: "sel" + parent.modelData; x: 150; level: parent.modelData; text: "Heading" }
            }
        }
        SelectableText { id: plain; objectName: "plain"; text: "plain" }
        TextEdit { id: bare; text: "plain"; visible: false }
    }

    Component.onCompleted: {
        probe.step("level matches Kirigami.Heading", null, function () {
            for (let l = 1; l <= 5; ++l) {
                const h = probe.find(probe.contentItem, it => it.objectName === "heading" + l);
                const s = probe.find(probe.contentItem, it => it.objectName === "sel" + l);
                probe.expect(h !== null && s !== null, "level " + l + " items missing");
                if (!h || !s) continue;
                probe.expect(s.level === l, "level property is " + s.level);
                probe.expect(Math.abs(s.font.pointSize - h.font.pointSize) < 0.01,
                             "level " + l + ": size " + s.font.pointSize + ", Heading " + h.font.pointSize);
                probe.expect(s.Accessible.role === Accessible.Heading, "level " + l + " role " + s.Accessible.role);
                probe.expect(s.activeFocusOnTab === false && s.readOnly === true && s.selectByMouse === true,
                             "level " + l + " lost its selectable-text behaviour");
            }
        });
        probe.step("level 0 is the old SelectableText", null, function () {
            probe.expect(plain.level === 0, "default level " + plain.level);
            probe.expect(plain.font.pointSize === bare.font.pointSize && plain.font.family === bare.font.family
                         && plain.font.weight === bare.font.weight,
                         "level 0 changed the font: " + plain.font.pointSize + " vs " + bare.font.pointSize);
            probe.expect(plain.Accessible.role === Accessible.StaticText, "level 0 role " + plain.Accessible.role);
        });
        probe.step("a leveled text is no Tab stop", null, function () {
            start.forceActiveFocus();
            probe.key(Qt.Key_Tab);
            probe.expect(!probe.find(probe.contentItem, it => it.objectName === "sel1").activeFocus,
                         "Tab reached a leveled text");
        });
    }
}
"#;

#[test]
fn level_matches_heading_size_and_role_and_level_zero_is_unchanged() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
