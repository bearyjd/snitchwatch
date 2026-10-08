//! The rule editor (`RuleEditorSheet.qml`, roadmap P2.1) headless, on a
//! `RulesPage` fed a rule list: "New rule…" opens it; the inspector's Edit
//! is unavailable for a read-only rule or one the editor can't express, and
//! opens the editor on the rule otherwise; a connection prefills it; the
//! precedence switch shows its warning; a refusal keeps the sheet open with
//! the reason as plain text, and only the bridge's confirmation closes it.
//!
//! Assertions are QML `throw`s, reported against the probe URL on stderr;
//! the Rust side fails on any such line (see `common::capture_stderr`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::ws_messages::{RuleCommandOutcome, ServerMessage};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rule_editor_sheet_probe.qml";

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

    function named(name) {
        return probeWindow.all(page.ruleEditor.contentItem, []).filter(function (i) {
            return i.objectName === name && i.visible;
        });
    }

    function rule(name, operator) {
        return { name: name, displayName: name, enabled: true, action: "deny",
                 duration: "always", description: "", operator: operator,
                 precedence: false, nolog: false };
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
                const locked = probeWindow.rule("z00-blocklist:ads:domains", curl);
                locked.readOnlyReason = "This rule belongs to a blocklist.";
                const hashed = probeWindow.rule("899-hashed", { type: "list", operand: "list",
                    operands: [curl, { type: "simple", operand: "process.hash.md5",
                                       data: "d41d8cd98f00b204e9800998ecf8427e" }] });
                rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules",
                    rules: [probeWindow.rule("899-curl", curl), locked, hashed] }));

                // "New rule…" opens an empty draft.
                probeWindow.expect(page.openEditor(""), "New rule didn't open");
                probeWindow.expect(sheet.opened, "the sheet isn't open");
                probeWindow.expect(controller.editingName === "", "a new rule names no rule");
                probeWindow.expect(sheet.draft.conditions.length === 0, "a new rule has conditions");
                // Without conditions it can't be saved, and says why.
                probeWindow.expect(probeWindow.named("editorProblem").length > 0, "no problem shown");
                probeWindow.expect(!probeWindow.named("editorSave")[0].enabled,
                    "Save enabled with problems");
                sheet.close();

                // Edit: unavailable for a read-only rule, and for one the
                // editor can't express; the reason is shown.
                probeWindow.expect(page.openRuleByName(locked.name), "locked rule not found");
                probeWindow.expect(!page.inspectorEditButton.enabled, "Edit enabled on a read-only rule");
                page.inspectorSheet.close();
                probeWindow.expect(page.openRuleByName("899-hashed"), "hashed rule not found");
                probeWindow.expect(!page.inspectorEditButton.enabled, "Edit enabled on a hash rule");
                probeWindow.expect(page.inspectNotEditable.indexOf("Edit isn't available") === 0,
                    "reason: " + page.inspectNotEditable);
                page.inspectorSheet.close();

                // Edit opens the editor on the rule.
                probeWindow.expect(page.openRuleByName("899-curl"), "curl rule not found");
                probeWindow.expect(page.inspectorEditButton.enabled, "Edit disabled on an editable rule");
                page.editInspected();
                probeWindow.expect(sheet.opened && !page.inspectorSheet.visible,
                    "Edit didn't swap the inspector for the editor");
                probeWindow.expect(controller.editingName === "899-curl", "editing " + controller.editingName);
                probeWindow.expect(sheet.draft.conditions.length === 1
                    && sheet.draft.conditions[0].value === "/usr/bin/curl", "draft not loaded");

                // Renaming says what happens.
                probeWindow.expect(probeWindow.named("editorRenameNote").length === 0, "rename note early");
                sheet.update({ name: "899-curl-renamed" });
                sheet.setCondition(0, { value: "/usr/bin/<b>curl</b>" });
                probeWindow.expect(probeWindow.named("editorValue")[0].text === "/usr/bin/<b>curl</b>",
                    "the value field follows the draft");
                probeWindow.expect(probeWindow.named("editorRenameNote").length === 1, "no rename note");

                // The precedence switch shows its warning.
                const decides = function () {
                    return probeWindow.named("editorWarning").some(function (w) {
                        return w.text.indexOf("Decides before other rules") === 0;
                    });
                };
                probeWindow.expect(!decides(), "precedence warning while off");
                sheet.showAdvanced = true;
                const precedence = probeWindow.named("editorPrecedence");
                probeWindow.expect(precedence.length === 1, "precedence switch not shown");
                precedence[0].toggle();
                precedence[0].toggled();
                probeWindow.expect(sheet.draft.precedence && decides(), "no precedence warning");

                // Loosening a rule takes a second click: the first names the
                // cautions, the second sends (here: nothing to send it to).
                sheet.setCondition(0, { value: "/usr/bin/curl" });
                sheet.update({ action: "allow" });
                probeWindow.expect(probeWindow.named("editorCaution").length > 0,
                    "no caution for turning a deny into an allow");
                const save = probeWindow.named("editorSave")[0];
                probeWindow.expect(save.enabled && save.text === "Save", "save: " + save.text);
                save.clicked();
                probeWindow.expect(save.text === "Save anyway", "save after one click: " + save.text);
                probeWindow.expect(controller.statusText.indexOf("Save anyway") >= 0,
                    "first click: " + controller.statusText);
                save.clicked();
                probeWindow.expect(controller.statusText.indexOf("isn't connected") >= 0,
                    "second click: " + controller.statusText);
                sheet.update({ description: "changed" });
                probeWindow.expect(save.text === "Save", "a change asks again: " + save.text);

                // A refusal keeps the sheet open, the reason as plain text; a
                // result for another request is ignored; only the bridge's
                // confirmation closes it.
                controller.statusText = "Not saved: <b>bad regexp</b>";
                probeWindow.expect(sheet.opened, "the sheet closed on a refusal");
                const status = probeWindow.named("editorStatus");
                probeWindow.expect(status.length === 1 && status[0].text === controller.statusText
                    && status[0].textFormat === Text.PlainText, "the reason isn't shown as plain text");
                // Every label showing markup shows it as text; checkboxes
                // carry none (Kirigami's own form labels are fixed text).
                let markup = 0;
                for (const node of probeWindow.all(sheet.contentItem, [])) {
                    if (node instanceof Controls.Label && node.text.indexOf("<b>") >= 0) {
                        markup += 1;
                        probeWindow.expect(node.textFormat === Text.PlainText,
                            "markup rendered as rich text: " + node.text);
                    }
                    if (node instanceof Controls.CheckBox) {
                        probeWindow.expect(node.text === "", "a checkbox carries text");
                    }
                }
                probeWindow.expect(markup >= 1, "markup-bearing labels checked: " + markup);
                controller.applyServerMessageJson(OTHER_RESULT_JSON);
                probeWindow.expect(sheet.opened, "another request's result closed the sheet");
                controller.saved();
                probeWindow.expect(!sheet.visible, "the confirmation didn't close the sheet");

                // A connection prefills the draft.
                probeWindow.expect(page.openEditor(JSON.stringify({ processPath: "/usr/bin/curl",
                    destHost: "example.com", destPort: 443 })), "prefill didn't open");
                probeWindow.expect(sheet.draft.conditions.length === 3,
                    "prefilled conditions: " + sheet.draft.conditions.length);
                probeWindow.expect(probeWindow.named("editorValue").length === 3, "condition rows");

                // Controls follow the draft after the user changed them: a
                // later draft or a changed operand must not leave a control
                // showing what isn't sent.
                const action = probeWindow.named("editorAction")[0];
                action.incrementCurrentIndex();
                probeWindow.expect(sheet.draft.action === "allow", "action: " + sheet.draft.action);
                const caseBox = probeWindow.named("editorCaseSensitive")[1];
                caseBox.toggle();
                caseBox.toggled();
                probeWindow.expect(sheet.draft.conditions[1].caseSensitive, "case not taken");
                sheet.chooseOperand(1, "dest.ip");
                probeWindow.expect(!sheet.draft.conditions[1].caseSensitive && !caseBox.checked,
                    "the checkbox didn't follow the operand");
                probeWindow.expect(sheet.startNew(), "New rule didn't open again");
                probeWindow.expect(action.currentIndex === 0 && sheet.draft.action === "deny",
                    "the action list didn't follow the new draft");

                // A typed time keeps its field while it passes a preset.
                const lasts = probeWindow.named("editorDuration")[0];
                lasts.activated(lasts.count - 1);
                probeWindow.expect(lasts.currentIndex === lasts.count - 1, "not on Custom time");
                const custom = probeWindow.named("editorCustomDuration");
                probeWindow.expect(custom.length === 1, "no field for a typed time");
                sheet.update({ duration: "1h" });
                probeWindow.expect(probeWindow.named("editorCustomDuration").length === 1,
                    "typing 1h hid the field");
                probeWindow.expect(lasts.currentIndex === lasts.count - 1, "1h took the preset");

            } finally {
                Qt.quit();
            }
        }
    }
}
"#;

#[test]
fn the_editor_opens_edits_warns_and_waits_for_the_bridge() {
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

    // A real result for a request this controller never sent, built from
    // the type so the probe can't pass on a message that fails to parse.
    let other_result = serde_json::to_string(&ServerMessage::RuleCommandResult {
        request_id: "someone-else".into(),
        outcome: RuleCommandOutcome::Ok,
    })
    .unwrap();
    let probe = PROBE.replace(
        "OTHER_RESULT_JSON",
        &serde_json::to_string(&other_result).unwrap(),
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
        "RuleEditorSheet probe failed to load (QML parse error):\n{captured}"
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
        "RuleEditorSheet probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
