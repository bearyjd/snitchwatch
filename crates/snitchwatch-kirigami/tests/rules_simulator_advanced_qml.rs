//! The Simulate sheet (`RuleSimulatorSheet.qml`), end to end: the "Advanced
//! inputs" fields reach `rules::simulator` through
//! `RulesModel.simulate(formJson)` and its result comes back into the sheet's
//! `simulate*` properties.
//!
//! The Qt-free table tests in `rules::simulator` cover the matching; this
//! probe covers what they can't — that the sheet's JSON keys line up with
//! `SimulationForm`'s, and that a blank advanced field is unknown (the rule is
//! reported as not evaluated) while a typed one decides. Assertions are QML
//! `throw`s, which Qt reports against the probe URL on stderr; the Rust side
//! fails on any such line (see `common::capture_stderr`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_simulator_advanced_probe.qml";

#[test]
fn advanced_inputs_reach_the_simulator_and_blank_ones_are_unknown() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 900
    height: 700

    function rule(name, action, type, operand, data) {
        return { name: name, displayName: name, enabled: true, action: action,
                 duration: "always", description: "",
                 operator: { type: type, operand: operand, data: data,
                             sensitive: false, list: [] },
                 precedence: false, nolog: false };
    }

    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    // The sheet's fields carry an objectName; find them through the item
    // tree instead of exporting them from the component.
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probeWindow.find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    function field(name) {
        const item = probeWindow.find(sheet.contentItem, name);
        probeWindow.expect(item !== null, "field not found: " + name);
        return item;
    }

    Item {
        anchors.fill: parent
        RuleSimulatorSheet {
            id: sheet
            model: RulesModel { id: rulesModel }
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                // No transitions, so open() takes effect at once.
                sheet.enter = null;
                sheet.exit = null;
                sheet.open();
                const uid = probeWindow.field("simUid");
                const checksums = probeWindow.field("simChecksums");
                const destIp = probeWindow.field("simDestIp");

                // A blank user ID is unknown: the deny that needs it is not
                // evaluated, the allow still decides, and the sheet says so
                // and makes its headline conditional.
                probeWindow.setRules([
                    probeWindow.rule("100-deny-uid", "deny", "simple", "user.id", "1000"),
                    probeWindow.rule("200-allow-tcp", "allow", "simple", "protocol", "tcp")
                ]);
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateRan, "simulation did not run");
                probeWindow.expect(sheet.simulateMatchedRule === "200-allow-tcp",
                    "blank uid: wrong rule: " + sheet.simulateMatchedRule);
                probeWindow.expect(sheet.simulateUnevaluated.indexOf("100-deny-uid") >= 0
                                   && sheet.simulateUnevaluated.indexOf("user ID") >= 0,
                    "blank uid: rule not reported as not evaluated: " + sheet.simulateUnevaluated);
                probeWindow.expect(sheet.simulateUndecided, "blank uid: headline not conditional");

                // Typing the uid decides it.
                uid.text = "1000";
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "100-deny-uid"
                                   && sheet.simulateAction === "deny",
                    "uid 1000: " + sheet.simulateMatchedRule + "/" + sheet.simulateAction);
                probeWindow.expect(sheet.simulateUnevaluated === "" && !sheet.simulateUndecided,
                    "uid 1000: still not evaluated: " + sheet.simulateUnevaluated);

                uid.text = "1001";
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "200-allow-tcp",
                    "uid 1001: " + sheet.simulateMatchedRule);
                probeWindow.expect(sheet.simulateUnevaluated === "",
                    "uid 1001: not evaluated: " + sheet.simulateUnevaluated);
                uid.text = "";

                // Hash conditions: with the checksum setting unknown (the
                // default) the rule is not a default match; with checksums
                // off, the daemon's match-everything note.
                probeWindow.setRules([
                    probeWindow.rule("100-hash", "deny", "simple", "process.hash.md5", "deadbeef")
                ]);
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === ""
                                   && sheet.simulateUnevaluated.indexOf("checksums are on") >= 0,
                    "hash, checksums unknown: " + sheet.simulateMatchedRule + " / " + sheet.simulateUnevaluated);
                checksums.currentIndex = 1;
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "100-hash"
                                   && sheet.simulateWarnings.indexOf("while checksums are off") >= 0,
                    "hash, checksums off: " + sheet.simulateMatchedRule + " / " + sheet.simulateWarnings);
                checksums.currentIndex = 0;

                // Text that isn't an IP address is reported as not valid,
                // not as left blank.
                probeWindow.setRules([
                    probeWindow.rule("100-ip", "deny", "simple", "dest.ip", "10.0.0.1")
                ]);
                destIp.text = "not-an-ip";
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateInvalid.indexOf("100-ip") >= 0
                                   && sheet.simulateUnevaluated === "",
                    "invalid ip: " + sheet.simulateInvalid + " / " + sheet.simulateUnevaluated);
                destIp.text = "10.0.0.1";
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "100-ip"
                                   && sheet.simulateInvalid === "",
                    "valid ip: " + sheet.simulateMatchedRule + " / " + sheet.simulateInvalid);
                destIp.text = "";

                // user.name cannot be simulated; it is said so, not guessed.
                probeWindow.setRules([
                    probeWindow.rule("100-name", "deny", "simple", "user.name", "alice")
                ]);
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "",
                    "user.name matched: " + sheet.simulateMatchedRule);
                probeWindow.expect(sheet.simulateUnsupported.indexOf("user.name") >= 0,
                    "user.name not listed as unsupported: " + sheet.simulateUnsupported);
            } finally {
                Qt.quit();
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
        // Without a root object nothing calls Qt.quit(): skip the loop and
        // let the assertion below report the load failure instead of hanging.
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RuleSimulatorSheet probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RuleSimulatorSheet probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}

/// Every result is shown as a simulation, and the sheet says what it can't
/// tell: these are fixed text in the sheet's source.
#[test]
fn the_sheet_labels_results_as_simulations() {
    let sheet: String = include_str!("../qml/RuleSimulatorSheet.qml")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for needed in [
        "Simulated result",
        "not a live daemon verdict",
        "Advanced inputs",
        "blank field is unknown",
        "If the rules below don't match",
        "(RE2)",
        "alias file on the daemon host may differ",
        "Once you type a variable here",
        "usually asks you",
    ] {
        assert!(
            sheet.contains(needed),
            "RuleSimulatorSheet.qml lost the simulator text `{needed}`"
        );
    }
    // The daemon doesn't always prompt, so the sheet must not say it does.
    assert!(
        !sheet.contains("opensnitchd asks you"),
        "the no-match text claims every connection prompts"
    );
}
