//! The `source.network` gate on the GUI side (`DaemonPolicy`, capability
//! `daemonPolicyV1`), headless, on a real `RulesPage` and `RulesModel`:
//!
//! - an older bridge (no `daemonPolicy` message, `refuseSourceNetwork`
//!   unset): nothing changes: no note on the list, the operand is offered,
//!   a `source.network` draft can be saved, and the simulator matches it;
//! - a bridge whose daemon refuses it: the enabled `source.network` rule's
//!   row says the firewall doesn't load it (not the disabled one, not
//!   `dest.network`); the editor's picker hides the operand on a new
//!   condition but keeps it on the rule's own condition, which stays
//!   editable and is refused in the bridge's words until it is changed; the
//!   simulator says why it can't match.
//!
//! Headless the controller has no bridge runtime: the probe sets
//! `refuseSourceNetwork` itself (live, it's read from
//! `BridgeHandles::daemon_policy`). Assertions are QML `throw`s, reported
//! against the probe URL on stderr; the Rust side fails on any such line.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::ws_messages::{DaemonPolicy, ServerMessage};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/source_network_gate_probe.qml";

const PROBE: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 900
    height: 900

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function all(item, out) {
        if (!item) return out;
        out.push(item);
        for (let i = 0; i < item.children.length; i++) {
            probeWindow.all(item.children[i], out);
        }
        return out;
    }

    function namedIn(root, name) {
        return probeWindow.all(root, []).filter(function (i) {
            return i.objectName === name && i.visible;
        });
    }

    function rule(name, enabled, operator) {
        return { name: name, displayName: name, enabled: enabled, action: "deny",
                 duration: "always", description: "", operator: operator,
                 precedence: false, nolog: false };
    }

    function findChild(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const found = probeWindow.findChild(kids[i], name);
            if (found) return found;
        }
        return item.contentItem ? probeWindow.findChild(item.contentItem, name) : null;
    }

    // The rows' visible "not loaded" notes (same row lookup as
    // `rules_all_apps_qml.rs`).
    function notes() {
        page.rulesList.forceLayout();
        const out = [];
        for (let i = 0; i < page.rulesList.count; i++) {
            const label = probeWindow.findChild(page.rulesList.itemAtIndex(i), "notLoadedReason");
            if (label && label.visible) out.push(label.text);
        }
        return out;
    }

    function hasSourceNetwork(combo) {
        for (let i = 0; i < combo.count; i++) {
            if (combo.textAt(i) === "Source: Network") return true;
        }
        return false;
    }

    function simulated() {
        return JSON.parse(rulesModel.simulate(JSON.stringify({
            processPath: "/usr/bin/curl", destIp: "10.0.0.1", srcIp: "10.0.0.2", destPort: 443 })));
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
                const sheet = page.ruleEditor;
                const controller = sheet.controller;
                sheet.enter = null;
                sheet.exit = null;
                page.inspectorSheet.enter = null;
                page.inspectorSheet.exit = null;

                const curl = { type: "simple", operand: "process.path",
                               data: "/usr/bin/curl", sensitive: true };
                const net = function (operand) {
                    return { type: "network", operand: operand, data: "10.0.0.0/8" };
                };
                const both = function (operand) {
                    return { type: "list", operand: "list", operands: [curl, net(operand)] };
                };
                rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: [
                    probeWindow.rule("899-lan-in", true, both("source.network")),
                    probeWindow.rule("899-lan-off", false, both("source.network")),
                    probeWindow.rule("899-lan-out", true, both("dest.network")) ] }));

                // An older bridge: today's behaviour.
                probeWindow.expect(!controller.refuseSourceNetwork, "refusing before any policy");
                probeWindow.expect(probeWindow.notes().length === 0, "notes: " + probeWindow.notes());
                let result = probeWindow.simulated();
                probeWindow.expect(result.matchedRule === "899-lan-in",
                    "old bridge simulation: " + JSON.stringify(result));
                probeWindow.expect(page.openEditor(""), "New rule didn't open");
                sheet.addCondition();
                probeWindow.expect(probeWindow.hasSourceNetwork(
                    probeWindow.namedIn(sheet.contentItem, "editorOperand")[0]),
                    "the operand is hidden on an older bridge");
                sheet.update({ name: "899-new" });
                sheet.setCondition(0, { value: "/usr/bin/curl" });
                sheet.addCondition();
                sheet.setCondition(1, { operand: "source.network", kind: "network",
                                        value: "10.0.0.0/8", caseSensitive: false });
                probeWindow.expect(sheet.checked.problems.length === 0,
                    "old bridge problems: " + sheet.checked.problems);
                sheet.close();

                // The daemon refuses it.
                rulesModel.applyServerMessageJson(DAEMON_POLICY_JSON);
                const shown = probeWindow.notes();
                probeWindow.expect(shown.length === 1 && shown[0].indexOf("doesn't load this rule") > 0,
                    "notes: " + JSON.stringify(shown));
                result = probeWindow.simulated();
                probeWindow.expect(result.matchedRule === "899-lan-out",
                    "the refused rule still decides: " + JSON.stringify(result));
                probeWindow.expect(result.unsupportedOperands.some(function (u) {
                    return u.reason === "opensnitchd 1.9 and newer refuses to load this rule";
                }), "no reason: " + JSON.stringify(result));

                controller.refuseSourceNetwork = true;
                probeWindow.expect(page.openEditor(""), "New rule didn't open again");
                sheet.addCondition();
                probeWindow.expect(!probeWindow.hasSourceNetwork(
                    probeWindow.namedIn(sheet.contentItem, "editorOperand")[0]),
                    "the operand is offered to a daemon that refuses it");
                sheet.close();

                // The rule's own condition keeps it, and stays editable.
                probeWindow.expect(page.openRuleByName("899-lan-in"), "rule not found");
                probeWindow.expect(page.inspectorEditButton.enabled, "Edit disabled");
                page.editInspected();
                probeWindow.expect(sheet.opened && controller.editingName === "899-lan-in",
                    "editing " + controller.editingName);
                const operands = probeWindow.namedIn(sheet.contentItem, "editorOperand");
                probeWindow.expect(operands.length === 2 && !probeWindow.hasSourceNetwork(operands[0])
                    && probeWindow.hasSourceNetwork(operands[1])
                    && operands[1].currentText === "Source: Network",
                    "operand pickers: " + operands.map(function (c) { return c.currentText; }));
                probeWindow.expect(sheet.checked.problems.some(function (p) {
                    // A list member's problem is prefixed with its place.
                    return p.indexOf("opensnitchd 1.9 and newer") >= 0;
                }), "not refused: " + JSON.stringify(sheet.checked.problems));
                probeWindow.expect(!probeWindow.namedIn(sheet.contentItem, "editorSave")[0].enabled,
                    "Save enabled on a refused rule");
                sheet.chooseOperand(1, "dest.network");
                probeWindow.expect(sheet.draft.conditions[1].kind === "network"
                    && sheet.checked.problems.length === 0,
                    "dest.network refused: " + JSON.stringify(sheet.checked.problems));
                sheet.close();

                // A new session starts from the default again.
                rulesModel.applyServerMessageJson(JSON.stringify({ action: "authenticated",
                                                                   capabilities: [] }));
                probeWindow.expect(probeWindow.notes().length === 0,
                    "a reconnect kept the notes: " + probeWindow.notes());
            } finally {
                Qt.quit();
            }
        }
    }
}
"#;

#[test]
fn source_network_follows_the_daemon_policy_and_an_old_bridge_changes_nothing() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    // Built from the type, so the probe can't pass on a message that fails
    // to parse.
    let policy = serde_json::to_string(&ServerMessage::DaemonPolicy(DaemonPolicy {
        refuse_source_network: true,
        daemon_version: Some("1.9".into()),
    }))
    .unwrap();
    let probe = PROBE.replace(
        "DAEMON_POLICY_JSON",
        &serde_json::to_string(&policy).unwrap(),
    );

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(probe.as_str()), &QUrl::from(PROBE_URL));
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "source.network gate probe failed to load (QML parse error):\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains("RuleEditorSheet.qml")
                || line.contains("RulesPage.qml")
        })
        .collect();
    assert!(
        bad_lines.is_empty(),
        "source.network gate probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
