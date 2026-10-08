//! Issue #48: the Rules inspector's "Enabled" switch never settles on a
//! state the daemon doesn't hold.
//!
//! The bridge re-sends the whole rule list (`SetRules`) after every rule
//! command, whether the daemon accepted it, refused it or never answered,
//! and clears it when its daemon stream goes away. The probe drives the
//! real `RulesPage.qml` + `RulesModel` through those cases using the
//! switch's own `toggle()`/`toggled()` (which, like a click, set `checked`
//! imperatively and would break a plain `checked:` binding):
//!
//!   * a click sends the value the switch shows, and a quick second click
//!     sends the opposite one — not a flip of the not-yet-updated model;
//!   * a list that still has the old value (refused / timed out) puts the
//!     switch back;
//!   * a list without the rule closes the sheet;
//!   * a rule the bridge marks read-only keeps its row and reason, but its
//!     controls are disabled and no command is emitted for it.
//!
//! Assertions are QML `throw`s, which Qt reports against the probe URL on
//! stderr; the Rust side fails on any such line (see `common::capture_stderr`
//! and `inline_verdict_qml.rs` for why that makes them load-bearing).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_inspector_probe.qml";

#[test]
fn inspector_switch_follows_the_bridges_rule_list() {
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
    width: 800
    height: 600

    property var sent: []

    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }

    function curl(enabled) {
        return { name: "899-curl", displayName: "899-curl", enabled: enabled,
                 action: "allow", duration: "always", description: "",
                 operator: { operand: "dest.host", data: "example.com" },
                 precedence: false, nolog: false };
    }

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function click(control) {
        control.toggle();
        control.toggled();
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    Connections {
        target: rulesModel
        function onRuleChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const sw = page.inspectorEnabledSwitch;
                // No transitions, so open()/close() take effect at once.
                page.inspectorSheet.enter = null;
                page.inspectorSheet.exit = null;
                probeWindow.setRules([probeWindow.curl(true)]);
                expect(page.openRuleByName("899-curl"), "rule not found");
                expect(page.inspectorSheet.opened, "inspector did not open");

                // Refused (or timed out): the bridge re-sends the old value.
                probeWindow.click(sw);
                expect(probeWindow.sent.length === 1 && probeWindow.sent[0].rule.enabled === false,
                       "first click did not send enabled=false: " + JSON.stringify(probeWindow.sent));
                probeWindow.setRules([probeWindow.curl(true)]);
                expect(sw.checked === true, "switch stayed off after the daemon refused");

                // Two quick clicks before any list arrives: off, then on.
                probeWindow.click(sw);
                probeWindow.click(sw);
                expect(probeWindow.sent.length === 3, "expected three commands");
                expect(probeWindow.sent[1].rule.enabled === false
                       && probeWindow.sent[2].rule.enabled === true,
                       "quick second click inverted the rule: " + JSON.stringify(probeWindow.sent));
                // The daemon ends up with the first one only.
                probeWindow.setRules([probeWindow.curl(false)]);
                expect(sw.checked === false && page.inspectEnabled === false,
                       "switch does not show the daemon's state");

                // A JS assignment breaks a plain `checked:` binding; the
                // switch must still follow the daemon's list afterwards.
                sw.checked = true;
                sw.toggled();
                expect(probeWindow.sent[3].rule.enabled === true, "assignment not sent");
                probeWindow.setRules([probeWindow.curl(false)]);
                expect(sw.checked === false, "switch lost its binding to the daemon's state");

                // The list no longer has the rule (deleted, or withdrawn).
                probeWindow.setRules([]);
                expect(!page.inspectorSheet.visible, "inspector stayed open on a vanished rule");

                // A rule Snitchwatch can't edit stays listed with its
                // reason; no control emits a command for it.
                const locked = probeWindow.curl(true);
                locked.name = "stock\\ui";
                locked.displayName = "stock\\ui";
                locked.readOnlyReason = "Snitchwatch can't edit this rule.";
                probeWindow.setRules([locked]);
                expect(page.openRuleByName("stock\\ui"), "read-only rule not listed");
                expect(page.inspectReadOnlyReason === locked.readOnlyReason, "reason not shown");
                expect(!sw.enabled, "switch enabled on a read-only rule");
                const before = probeWindow.sent.length;
                page.setInspectEnabled(false);
                rulesModel.deleteRule("stock\\ui");
                expect(probeWindow.sent.length === before,
                       "a command was emitted for a read-only rule: " + JSON.stringify(probeWindow.sent));
                expect(!page.inspectorDeleteButton.enabled, "Delete enabled on a bad-name rule");

                // Read-only only for its conditions: no toggle, but Delete
                // works (a delete names the rule and nothing else). The
                // bad-name rule is opened in between, through both fill
                // paths, so a value left over from the last rule shows up.
                const shape = probeWindow.curl(true);
                shape.name = "899-lan";
                shape.displayName = "899-lan";
                shape.readOnlyReason = "Snitchwatch can't change this rule.";
                shape.deletable = true;
                locked.deletable = false;
                probeWindow.setRules([shape, locked]);
                expect(page.openRuleByName("899-lan"), "shape-only rule not listed");
                expect(!sw.enabled, "switch enabled on a shape-only read-only rule");
                expect(page.inspectorDeleteButton.enabled, "Delete disabled on a shape-only rule");
                expect(page.openRuleByName("stock\\ui"), "bad-name rule not listed");
                expect(!page.inspectorDeleteButton.enabled, "stale Delete on a bad-name rule");
                const rows = page.rulesList;
                expect(rows.count === 2, "rows not created: " + rows.count);
                rows.currentIndex = 0;
                expect(rows.currentItem !== null, "no delegate for the shape-only rule");
                page.openInspector(rows.currentItem);
                expect(page.inspectorDeleteButton.enabled, "row path: Delete disabled on a shape-only rule");
                rows.currentIndex = 1;
                page.openInspector(rows.currentItem);
                expect(!page.inspectorDeleteButton.enabled, "row path: Delete enabled on a bad-name rule");
                const beforeShape = probeWindow.sent.length;
                page.setInspectEnabled(false);
                rulesModel.setEnabled("899-lan", false);
                expect(probeWindow.sent.length === beforeShape, "a change was emitted for a read-only rule");
                rulesModel.deleteRule("stock\\ui");
                expect(probeWindow.sent.length === beforeShape, "a delete was emitted for a bad-name rule");
                rulesModel.deleteRule("899-lan");
                expect(probeWindow.sent.length === beforeShape + 1
                       && probeWindow.sent[beforeShape].action === "deleteRule",
                       "no delete for a shape-only rule: " + JSON.stringify(probeWindow.sent));

                // Prompt-slot D: a recommended rule is read-only and can't be
                // deleted, but the bridge says it can be turned on or off.
                const curated = probeWindow.curl(true);
                curated.name = "snitchwatch-default-flatpak-flathub";
                curated.displayName = curated.name;
                curated.readOnlyReason = "A recommended background-service rule.";
                curated.deletable = false;
                curated.toggleable = true;
                probeWindow.setRules([curated, locked]);
                expect(page.openRuleByName(curated.name), "recommended rule not listed");
                expect(sw.enabled, "switch disabled on a recommended rule");
                expect(!page.inspectorDeleteButton.enabled, "Delete enabled on a recommended rule");
                const beforeCurated = probeWindow.sent.length;
                probeWindow.click(sw);
                expect(probeWindow.sent.length === beforeCurated + 1
                       && probeWindow.sent[beforeCurated].rule.enabled === false
                       && probeWindow.sent[beforeCurated].ruleId === curated.name,
                       "no toggle for a recommended rule: " + JSON.stringify(probeWindow.sent));
                expect(page.openRuleByName("stock\\ui"), "bad-name rule not listed");
                expect(!sw.enabled, "stale switch on a read-only rule");
                rows.currentIndex = 0;
                page.openInspector(rows.currentItem);
                expect(sw.enabled, "row path: switch disabled on a recommended rule");
                rows.currentIndex = 1;
                page.openInspector(rows.currentItem);
                expect(!sw.enabled, "row path: stale switch on a read-only rule");
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
        "RulesPage inspector probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage inspector probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
