//! `RuleButton.qml` on its own: each `levelRuleState` gives its colour, glyph,
//! label and enabled state; the label and glyph are drawn (colour is never
//! the only cue); a click opens Allow/Deny for `none` and "Remove rule" for
//! `allow`/`deny`, and does nothing for `partial` and `pending`. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rule_button_probe.qml";
const DONE_MARKER: &str = "RULE_BUTTON_PROBE_DONE";

#[test]
fn rule_button_states_and_clicks() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 400
    height: 200
    property var failures: []
    property var requests: []
    property int phase: 0

    RuleButton {
        id: btn
        x: 20
        y: 20
        level: 1
        onCreateRequested: choice => probe.requests.push("create:" + choice)
        onRemoveRequested: probe.requests.push("remove")
    }
    TestCase { id: tc; name: "rulebutton"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    // state -> [colour, glyph, label, enabled]
    readonly property var table: ({
        allow: [Theme.allow, "\u2713", "Allowed", true],
        deny: [Theme.deny, "\u2715", "Denied", true],
        partial: [Theme.partial, "\u25D0", "Partial", false],
        none: [Theme.needsRule, "+", "Needs rule", true],
        pending: [Theme.pending, "\u25F7", "Waiting for the firewall", false]
    })

    function checkTable() {
        const seenGlyphs = [];
        const seenLabels = [];
        for (const s of Object.keys(probe.table)) {
            btn.ruleState = s;
            const want = probe.table[s];
            probe.expect(Qt.colorEqual(btn.stateColor, want[0]), s + " colour");
            probe.expect(btn.glyph === want[1], s + " glyph " + btn.glyph);
            probe.expect(btn.stateLabel === want[2], s + " label " + btn.stateLabel);
            probe.expect(btn.button.enabled === want[3], s + " enabled " + btn.button.enabled);
            probe.expect(btn.tipText.length > 0, s + " tooltip");
            // The drawn glyph and label are the ones in the table, so the
            // state reads without its colour.
            const shown = btn.button.contentItem.children;
            probe.expect(shown[0].text === want[1] && shown[1].text === want[2], s + " drawn text");
            probe.expect(btn.button.Accessible.name.indexOf(want[2]) >= 0
                         && btn.button.Accessible.name.indexOf("domain") >= 0, s + " accessible name: " + btn.button.Accessible.name);
            probe.expect(seenGlyphs.indexOf(btn.glyph) < 0, s + " glyph not distinct");
            probe.expect(seenLabels.indexOf(btn.stateLabel) < 0, s + " label not distinct");
            seenGlyphs.push(btn.glyph);
            seenLabels.push(btn.stateLabel);
        }
        btn.ruleState = "bogus";
        probe.expect(btn.stateLabel === "Needs rule", "unknown state falls back to none");
        btn.level = 0;
        probe.expect(btn.tipText.indexOf("program") >= 0, "tooltip names the level");
        btn.level = 1;
        probe.expect(btn.table.partial.tip.indexOf("Rules page") >= 0, "partial tip points at the Rules page");
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    probe.checkTable();
                } else if (probe.phase === 1) {
                    btn.ruleState = "none";
                    tc.mouseClick(btn.button);
                } else if (probe.phase === 2) {
                    probe.expect(btn.chooseMenu.visible, "none: choose menu did not open");
                    probe.expect(!btn.removeMenu.visible, "none: remove menu opened");
                    btn.chooseMenu.itemAt(1).triggered();
                    btn.chooseMenu.close();
                } else if (probe.phase === 3) {
                    btn.ruleState = "deny";
                    tc.mouseClick(btn.button);
                } else if (probe.phase === 4) {
                    probe.expect(btn.removeMenu.visible, "deny: remove menu did not open");
                    probe.expect(btn.removeMenu.itemAt(0).text === "Remove rule", "remove item text");
                    btn.removeMenu.itemAt(0).triggered();
                    btn.removeMenu.close();
                } else if (probe.phase === 5) {
                    for (const s of ["partial", "pending"]) {
                        btn.ruleState = s;
                        tc.mouseClick(btn.button);
                        probe.expect(!btn.chooseMenu.visible && !btn.removeMenu.visible, s + ": a menu opened");
                    }
                    probe.expect(probe.requests.join(",") === "create:deny,remove", "requests: " + probe.requests.join(","));
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
                        throw new Error("rule button probe: " + probe.failures.join("; "));
                    }
                    console.warn("RULE_BUTTON_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
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

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "the rule button did not behave:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
