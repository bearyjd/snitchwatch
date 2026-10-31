//! Selectable text, batch 3: the Learning review row's right-click Copy
//! carries EVERY text the row shows (warnings and per-choice notes included),
//! checked on a real `LearningReviewRow` rather than by reading its source.
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

const PROBE_URL: &str = "qrc:/selectable_batch3_row_copy_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_BATCH3_ROW_COPY_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 700
    height: 700
    readonly property string doneMarker: "SELECTABLE_BATCH3_ROW_COPY_DONE"
    // @a11y-guard
    // @list-keys

    readonly property var full: ({
        key: "k1", programLabel: "curl", programLabelUnverified: false,
        programPathFull: "/usr/bin/curl", connections: 3, distinct: 2, distinctExact: true,
        lastSeenMs: 1000, flags: ["FLAGTEXT"], top: [{ host: "a.example", count: 2 }],
        destinationsHidden: false, dualUse: true, highFanOut: true,
        notSelectable: "NOTSELECTABLETEXT", alreadyPresent: ["block"],
        choices: [{ kind: "allow-hosts", title: "CHOICETITLE", hosts: ["h.example"], note: "CHOICENOTE" }]
    })
    readonly property var bare: ({
        key: "k2", programLabel: "ls", programLabelUnverified: false,
        programPathFull: "/usr/bin/ls", connections: 1, distinct: 1, distinctExact: true,
        lastSeenMs: 1000, flags: [], top: [], destinationsHidden: false,
        dualUse: false, highFanOut: false, notSelectable: null, alreadyPresent: [],
        choices: [{ kind: "block", title: "BARETITLE", hosts: [], note: null }]
    })

    LearningReviewRow { id: rowFull; x: 0; y: 0; width: 600; info: probe.full }
    LearningReviewRow { id: rowBare; x: 0; y: 350; width: 600; info: probe.bare }

    function handlerOf(item) {
        for (let i = 0; i < item.data.length; ++i) if (item.data[i].objectName === "rowCopyMenu") return item.data[i];
        return null;
    }

    Component.onCompleted: {
        probe.step("copy text has every visible text", null, function () {
            const text = probe.handlerOf(rowFull).copyText;
            const lines = text.split("\n");
            for (const want of ["curl", "/usr/bin/curl", rowFull.countsText, "FLAGTEXT",
                                "This program can run other programs or scripts.",
                                "It connected to 2 or more destinations, so no destination list is chosen for it.",
                                "a.example  (2)", "NOTSELECTABLETEXT",
                                "A rule you already have makes: block this program.",
                                "CHOICETITLE", "Only: h.example", "CHOICENOTE"])
                probe.expect(lines.indexOf(want) >= 0, "copy text lacks '" + want + "': " + JSON.stringify(text));
            const order = ["FLAGTEXT", "can run other", "or more destinations", "a.example", "NOTSELECTABLETEXT",
                           "already have", "CHOICETITLE", "Only: h.example", "CHOICENOTE"].map(w => text.indexOf(w));
            probe.expect(JSON.stringify(order) === JSON.stringify(order.slice().sort((a, b) => a - b)),
                         "copy text is not in the shown order: " + JSON.stringify(order));
        });
        probe.step("hidden texts stay out", null, function () {
            const text = probe.handlerOf(rowBare).copyText;
            probe.expect(text.indexOf("can run other") < 0 && text.indexOf("or more destinations") < 0
                         && text.indexOf("already have") < 0 && text.indexOf("Only:") < 0,
                         "a hidden text leaked into the copy: " + JSON.stringify(text));
            probe.expect(text.split("\n").indexOf("BARETITLE") >= 0, "title missing: " + JSON.stringify(text));
            probe.expect(text.indexOf("\n\n") < 0 && !text.endsWith("\n"), "blank lines in the copy: " + JSON.stringify(text));
        });
    }
}
"#;

#[test]
fn row_copy_text_has_every_visible_text() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
