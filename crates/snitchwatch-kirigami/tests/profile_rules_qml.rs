//! The Profiles page's rules (issue #46 Part 2) headless: an active
//! profile's rules show whether the firewall installed them, as plain text;
//! "Add rule…" opens the rule editor in its profile mode and comes back;
//! Remove sends `removeProfileRule`; a bridge that applies no profile rules
//! says why.
//!
//! Assertions are QML `throw`s, reported against the probe URL on stderr;
//! the Rust side fails on any such line (see `common::capture_stderr`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/profile_rules_probe.qml";

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

    property var sent: []

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

    function named(root, name) {
        return probeWindow.all(root, []).filter(function (i) {
            return i.objectName === name && i.visible;
        });
    }

    function setProfiles(applies, reason) {
        profilesModel.applyServerMessageJson(JSON.stringify({
            action: "setProfiles", storage: { persistent: true },
            appliesRules: applies, notAppliedReason: reason,
            profiles: [{ id: "home", name: "Home", networkMatchers: [], active: true,
                rules: [
                    { id: "r1", action: "deny", operand: "dest.host", data: "<b>ads</b>.example",
                      enforcement: "rule_installed" },
                    { id: "r2", action: "allow", operand: "dest.host", data: "x.example",
                      enforcement: "not_enforced", enforcementReason: "Not installed: <i>no</i>" }
                ] }]
        }));
    }

    ProfilesPage {
        id: page
        anchors.fill: parent
        model: ProfilesModel { id: profilesModel }
    }

    Connections {
        target: profilesModel
        function onProfileChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const inspector = page.inspectorSheet;
                const editor = page.ruleEditor;
                inspector.enter = null;
                inspector.exit = null;
                editor.enter = null;
                editor.exit = null;
                probeWindow.setProfiles(true, null);
                page.openInspector({ profileId: "home", name: "Home", networkMatchers: "",
                                     isActive: true });
                probeWindow.expect(inspector.opened, "the inspector didn't open");

                // Each rule: what it matches and whether the firewall has it.
                const statuses = probeWindow.named(inspector.contentItem, "profileRuleStatus")
                    .map(function (l) { return l.text; });
                probeWindow.expect(statuses.length === 2 && statuses[0] === "Rule installed"
                    && statuses[1] === "Not installed", "statuses: " + statuses);
                const reasons = probeWindow.named(inspector.contentItem, "profileRuleReason");
                probeWindow.expect(reasons.length === 1
                    && reasons[0].text === "Not installed: <i>no</i>", "reason not shown");
                let markup = 0;
                for (const node of probeWindow.all(inspector.contentItem, [])) {
                    if (node instanceof Controls.Label && node.text.indexOf("<") >= 0) {
                        markup += 1;
                        probeWindow.expect(node.textFormat === Text.PlainText,
                            "markup rendered as rich text: " + node.text);
                    }
                }
                probeWindow.expect(markup >= 2, "markup-bearing labels: " + markup);

                // Remove sends removeProfileRule for that rule.
                probeWindow.named(inspector.contentItem, "profileRuleRemove")[1].clicked();
                const removed = probeWindow.sent[probeWindow.sent.length - 1];
                probeWindow.expect(removed.action === "removeProfileRule"
                    && removed.profileId === "home" && removed.ruleId === "r2",
                    "removed: " + JSON.stringify(removed));

                // Add rule: the editor's profile mode, then back.
                probeWindow.named(inspector.contentItem, "profileAddRule")[0].clicked();
                probeWindow.expect(editor.opened && !inspector.visible, "the editor didn't open");
                probeWindow.expect(editor.controller.profileId === "home", "not in profile mode");
                probeWindow.expect(editor.title === "New rule for this profile", editor.title);
                for (const hidden of ["editorName", "editorDuration", "editorEnabled",
                                      "editorShowAdvanced", "editorDescription"]) {
                    probeWindow.expect(probeWindow.named(editor.contentItem, hidden).length === 0,
                        hidden + " shown for a profile rule");
                }
                editor.addCondition();
                editor.setCondition(0, { value: "/usr/bin/curl" });
                probeWindow.expect(probeWindow.named(editor.contentItem, "editorProblem").length === 0,
                    "a valid profile rule has problems");
                editor.setCondition(0, { caseSensitive: false });
                probeWindow.expect(probeWindow.named(editor.contentItem, "editorProblem").length > 0,
                    "a case-folded path isn't refused");
                editor.setCondition(0, { caseSensitive: true });
                editor.controller.statusText = "Saved to the profile. <b>x</b>";
                editor.controller.saved();
                probeWindow.expect(!editor.visible && inspector.opened,
                    "the inspector didn't come back");
                const status = probeWindow.named(inspector.contentItem, "profileEditorStatus");
                probeWindow.expect(status.length === 1 && status[0].textFormat === Text.PlainText,
                    "the editor's result isn't shown as plain text");

                // A bridge that applies no profile rules says why.
                probeWindow.setProfiles(false, "per-user <b>mode</b>");
                let why = null;
                for (let i = 0; i < page.header.children.length; i++) {
                    if (page.header.children[i].objectName === "notAppliedReason") {
                        why = page.header.children[i];
                    }
                }
                probeWindow.expect(why && why.visible && why.text === "Why: per-user <b>mode</b>"
                    && why.textFormat === Text.PlainText, "the reason isn't shown");
            } finally {
                Qt.quit();
            }
        }
    }
}
"#;

#[test]
fn profile_rules_show_their_status_and_are_added_through_the_editor() {
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

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
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
        "profile rules probe failed to load (QML parse error):\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains("ProfilesPage.qml")
                || line.contains("RuleEditorSheet.qml")
        })
        .collect();
    assert!(
        bad_lines.is_empty(),
        "profile rules probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
