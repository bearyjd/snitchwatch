//! The Rules page's Simulate sheet, end to end: the "Advanced inputs" fields
//! reach `rules::simulator` through `RulesModel.simulate(formJson)` and its
//! result comes back into the page's `simulate*` properties.
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

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                // A blank user ID is unknown: the deny that needs it is not
                // evaluated, the allow still decides, and the page says so.
                probeWindow.setRules([
                    probeWindow.rule("100-deny-uid", "deny", "simple", "user.id", "1000"),
                    probeWindow.rule("200-allow-tcp", "allow", "simple", "protocol", "tcp")
                ]);
                page.runSimulation();
                probeWindow.expect(page.simulateRan, "simulation did not run");
                probeWindow.expect(page.simulateMatchedRule === "200-allow-tcp",
                    "blank uid: wrong rule: " + page.simulateMatchedRule);
                probeWindow.expect(page.simulateUnevaluated.indexOf("100-deny-uid") >= 0
                                   && page.simulateUnevaluated.indexOf("user ID") >= 0,
                    "blank uid: rule not reported as not evaluated: " + page.simulateUnevaluated);

                // Typing the uid decides it.
                page.simulateUidField.text = "1000";
                page.runSimulation();
                probeWindow.expect(page.simulateMatchedRule === "100-deny-uid"
                                   && page.simulateAction === "deny",
                    "uid 1000: " + page.simulateMatchedRule + "/" + page.simulateAction);
                probeWindow.expect(page.simulateUnevaluated === "",
                    "uid 1000: still not evaluated: " + page.simulateUnevaluated);

                page.simulateUidField.text = "1001";
                page.runSimulation();
                probeWindow.expect(page.simulateMatchedRule === "200-allow-tcp",
                    "uid 1001: " + page.simulateMatchedRule);
                probeWindow.expect(page.simulateUnevaluated === "",
                    "uid 1001: not evaluated: " + page.simulateUnevaluated);
                page.simulateUidField.text = "";

                // Hash conditions: unknown checksum setting -> a "may" note;
                // off -> the daemon's match-everything note.
                probeWindow.setRules([
                    probeWindow.rule("100-hash", "allow", "simple", "process.hash.md5", "deadbeef")
                ]);
                page.runSimulation();
                probeWindow.expect(page.simulateMatchedRule === "100-hash"
                                   && page.simulateWarnings.indexOf("may match every program") >= 0,
                    "hash, checksums unknown: " + page.simulateMatchedRule + " / " + page.simulateWarnings);
                page.simulateChecksumsBox.currentIndex = 1;
                page.runSimulation();
                probeWindow.expect(page.simulateWarnings.indexOf("while checksums are off") >= 0,
                    "hash, checksums off: " + page.simulateWarnings);
                page.simulateChecksumsBox.currentIndex = 0;

                // user.name cannot be simulated; it is said so, not guessed.
                probeWindow.setRules([
                    probeWindow.rule("100-name", "deny", "simple", "user.name", "alice")
                ]);
                page.runSimulation();
                probeWindow.expect(page.simulateMatchedRule === "",
                    "user.name matched: " + page.simulateMatchedRule);
                probeWindow.expect(page.simulateUnsupported.indexOf("user.name") >= 0,
                    "user.name not listed as unsupported: " + page.simulateUnsupported);
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
        "RulesPage simulator probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage simulator probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}

/// Every result is shown as a simulation, and the page says what it can't
/// tell: both strings are fixed text in the page source.
#[test]
fn the_sheet_labels_results_as_simulations() {
    let page: String = include_str!("../qml/RulesPage.qml")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for needed in [
        "Simulated result",
        "not a live daemon verdict",
        "Advanced inputs",
        "blank field is unknown",
    ] {
        assert!(
            page.contains(needed),
            "RulesPage.qml lost the simulator text `{needed}`"
        );
    }
}
