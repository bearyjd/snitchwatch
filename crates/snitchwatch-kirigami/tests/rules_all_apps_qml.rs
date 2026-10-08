//! Issue #44, second half (Part B): the Rules tab flags pre-#50 Snitchwatch
//! rules that apply to every app, says on each row what deleting it changes
//! (a deny row says it unblocks the host), and deletes exactly that rule in
//! one click. There is no bulk delete anywhere.
//!
//! Same probe shape as `rules_inspector_qml.rs`: a real `RulesPage` +
//! `RulesModel` fed `SetRules`, assertions thrown against the probe URL. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_all_apps_probe.qml";
const RULES_PAGE: &str = include_str!("../qml/RulesPage.qml");
const RULES_MODEL: &str = include_str!("../src/rules_model.rs");

fn code_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The owner's decision is one click per row, never "delete all": deleting a
/// deny can unblock traffic, so each deletion must stay a deliberate choice.
#[test]
fn rules_page_has_no_bulk_delete() {
    let page = code_lines(RULES_PAGE);
    let lowered = page.to_lowercase();
    for forbidden in ["delete all", "remove all", "deleteall", "clear all"] {
        assert!(
            !lowered.contains(forbidden),
            "RulesPage.qml offers a bulk delete (`{forbidden}`)"
        );
    }
    assert_eq!(
        page.matches("deleteRule(").count(),
        2,
        "deleteRule is called only by the inspector's confirm button and a row's Delete button"
    );
    assert_eq!(
        code_lines(RULES_MODEL)
            .matches("ClientMessage::DeleteRule")
            .count(),
        1,
        "RulesModel builds DeleteRule in exactly one place (`deleteRule(name)`)"
    );
}

#[test]
fn all_apps_rows_are_flagged_explained_and_deleted_one_at_a_time() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 1000
    height: 800

    property var sent: []
    property var failures: []

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }
    function rule(name, action, operator, readOnlyReason) {
        return { name: name, displayName: name, enabled: true, action: action,
                 duration: "always", description: "snitchwatch interactive verdict",
                 operator: operator, precedence: false, nolog: false,
                 readOnlyReason: readOnlyReason || null };
    }
    function host(h) {
        return { type: "simple", operand: "dest.host", data: h, sensitive: false };
    }
    function findChild(item, name) {
        if (!item) {
            return null;
        }
        if (item.objectName === name) {
            return item;
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const found = probeWindow.findChild(kids[i], name);
            if (found) {
                return found;
            }
        }
        return item.contentItem ? probeWindow.findChild(item.contentItem, name) : null;
    }
    function findText(item, text) {
        if (!item) {
            return null;
        }
        if (item.text === text) {
            return item;
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const found = probeWindow.findText(kids[i], text);
            if (found) {
                return found;
            }
        }
        return item.contentItem ? probeWindow.findText(item.contentItem, text) : null;
    }
    function rowItem(index) {
        page.rulesList.forceLayout();
        return page.rulesList.itemAtIndex(index);
    }
    function shown(index, name) {
        const child = probeWindow.findChild(probeWindow.rowItem(index), name);
        return child !== null && child.visible;
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
                const deny = "snitchwatch-deny-github.com-443";
                const allow = "snitchwatch-allow-pypi.org-1b4f0e9851971998-443";
                probeWindow.setRules([
                    probeWindow.rule(deny, "deny", probeWindow.host("github.com")),
                    probeWindow.rule(allow, "allow", probeWindow.host("pypi.org")),
                    probeWindow.rule(
                        "snitchwatch-allow-github.com-3aeb002460381c6f-443-pcurl-0123456789abcdef",
                        "allow",
                        { type: "list", operands: [
                            { type: "simple", operand: "process.path", data: "/usr/bin/curl",
                              sensitive: true },
                            probeWindow.host("github.com") ] }),
                    probeWindow.rule("snitchwatch-deny-locked", "deny",
                                     probeWindow.host("example.com"),
                                     "Snitchwatch can't edit this rule.")
                ]);

                probeWindow.check(rulesModel.legacyHostOnlyCount === 3,
                                  "legacyHostOnlyCount " + rulesModel.legacyHostOnlyCount);
                probeWindow.check(page.header.visible, "the all-apps notice is hidden");
                // Only Snitchwatch's own earlier rules are counted, so the
                // label must not compare them with every rule (code review C1).
                const counted = probeWindow.findChild(page.header, "allAppsCount");
                probeWindow.check(counted && counted.text ===
                    "3 rules saved by earlier Snitchwatch versions apply to all apps or to "
                    + "unidentified programs",
                    "count label: " + (counted ? counted.text : "missing"));

                const denyHint = probeWindow.findChild(probeWindow.rowItem(0), "allAppsHint");
                probeWindow.check(denyHint && denyHint.visible && denyHint.text ===
                    "Deleting this unblocks github.com for every app with an allow rule covering "
                    + "it, including rules for just this host; other apps will be asked.",
                    "deny row warning: " + (denyHint ? denyHint.text : "missing"));
                const allowHint = probeWindow.findChild(probeWindow.rowItem(1), "allAppsHint");
                probeWindow.check(allowHint && allowHint.visible && allowHint.text ===
                    "Deleting this makes every app ask again before reaching pypi.org.",
                    "allow row hint: " + (allowHint ? allowHint.text : "missing"));
                probeWindow.check(probeWindow.shown(0, "allAppsFlag")
                                  && probeWindow.shown(1, "allAppsFlag"),
                                  "flagged rows are not marked");

                // An app-bound (#50) rule is not flagged and has no row delete.
                probeWindow.check(!probeWindow.shown(2, "allAppsFlag")
                                  && !probeWindow.shown(2, "allAppsHint")
                                  && !probeWindow.shown(2, "allAppsDelete"),
                                  "app-bound rule flagged");
                // A read-only rule keeps its flag but offers no delete, and
                // its hint doesn't talk about deleting it (code review C5).
                probeWindow.check(probeWindow.shown(3, "allAppsFlag")
                                  && !probeWindow.shown(3, "allAppsDelete"),
                                  "read-only rule offers a delete");
                const lockedHint = probeWindow.findChild(probeWindow.rowItem(3), "allAppsHint");
                probeWindow.check(lockedHint && lockedHint.text ===
                    "This rule applies to every app. Snitchwatch can't delete it; its details say why.",
                    "read-only hint: " + (lockedHint ? lockedHint.text : "missing"));

                // One click, one DeleteRule, for exactly that row's rule.
                const del = probeWindow.findChild(probeWindow.rowItem(0), "allAppsDelete");
                probeWindow.check(del && del.visible, "deny row has no Delete button");
                if (del) {
                    del.clicked();
                }
                probeWindow.check(probeWindow.sent.length === 1
                                  && probeWindow.sent[0].action === "deleteRule"
                                  && probeWindow.sent[0].ruleId === deny,
                                  "Delete sent: " + JSON.stringify(probeWindow.sent));

                probeWindow.check(typeof rulesModel.deleteAll === "undefined"
                                  && typeof rulesModel.deleteAllAppsRules === "undefined",
                                  "RulesModel exposes a bulk delete");

                // #68: read-only for its shape but deletable by the bridge's
                // flag, so it gets its Delete button and the deleting hint.
                const shapeRefused = probeWindow.rule("snitchwatch-deny-shape", "deny",
                                                      probeWindow.host("example.org"),
                                                      "Snitchwatch can't edit this rule.");
                shapeRefused.deletable = true;
                probeWindow.setRules([shapeRefused]);
                const shapeHint = probeWindow.findChild(probeWindow.rowItem(0), "allAppsHint");
                probeWindow.check(probeWindow.shown(0, "allAppsDelete") && shapeHint
                                  && shapeHint.text.startsWith("Deleting this unblocks example.org"),
                                  "deletable read-only rule: delete "
                                  + probeWindow.shown(0, "allAppsDelete") + ", hint "
                                  + (shapeHint ? shapeHint.text : "missing"));

                probeWindow.setRules([probeWindow.rule(deny, "deny", probeWindow.host("github.com"))]);
                const one = probeWindow.findChild(page.header, "allAppsCount");
                probeWindow.check(one && one.text ===
                    "1 rule saved by an earlier Snitchwatch version applies to all apps or to an "
                    + "unidentified program",
                    "singular count label: " + (one ? one.text : "missing"));

                // Issue #64: tied to a "program" that isn't a program file.
                probeWindow.setRules([probeWindow.rule("snitchwatch-allow-x-443-pkernel", "allow",
                    { type: "list", operands: [
                        { type: "simple", operand: "process.path", data: "Kernel connection",
                          sensitive: true },
                        probeWindow.host("x.example") ] })]);
                const kernelFlag = probeWindow.findChild(probeWindow.rowItem(0), "allAppsFlag");
                const kernelHint = probeWindow.findChild(probeWindow.rowItem(0), "allAppsHint");
                probeWindow.check(kernelFlag && kernelFlag.visible
                                  && kernelFlag.text === "Program not identified"
                                  && probeWindow.shown(0, "allAppsDelete") && kernelHint
                                  && kernelHint.text.indexOf("isn't a program file") >= 0,
                                  "unidentified program: " + (kernelFlag ? kernelFlag.text : "")
                                  + " / " + (kernelHint ? kernelHint.text : "missing"));

                // Without flagged rules the notice goes away.
                probeWindow.setRules([]);
                probeWindow.check(rulesModel.legacyHostOnlyCount === 0 && !page.header.visible,
                                  "notice stays without flagged rules");

                // N4: an action the daemon doesn't recognise is shown as
                // written, with a note, in a neutral colour (row and inspector).
                probeWindow.setRules([probeWindow.rule("100-odd", "drop",
                                                       probeWindow.host("x.example"))]);
                const odd = "\"drop\" (unrecognised: blocks)";
                const oddLabel = probeWindow.findText(probeWindow.rowItem(0), odd);
                probeWindow.check(oddLabel !== null && oddLabel.textFormat === Text.PlainText
                                  && Qt.colorEqual(oddLabel.color, page.actionColor(odd))
                                  && Qt.colorEqual(page.actionColor(odd),
                                                   Kirigami.Theme.neutralTextColor),
                                  "unrecognised action label");
                probeWindow.check(page.openRuleByName("100-odd") && page.inspectAction === odd,
                                  "inspector action: " + page.inspectAction);
                page.inspectorSheet.close();
                probeWindow.setRules([]);

                // Issue #61: what the list leaves out is said under the title.
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "rulesNotShown", tooLarge: 2, listed: true }));
                const notShown = probeWindow.findChild(page.header, "rulesNotShown");
                probeWindow.check(page.header.visible && notShown && notShown.visible
                                  && notShown.textFormat === Text.PlainText
                                  && notShown.text.indexOf("2 rules aren't listed") === 0,
                                  "not-shown label: " + (notShown ? notShown.text : "missing"));
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "rulesNotShown", tooLarge: 0, listed: true }));
                probeWindow.check(!page.header.visible, "the not-shown label stays");
                // With none listed, "No rules yet" would contradict it.
                const placeholder = probeWindow.findChild(page, "rulesEmptyPlaceholder");
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "setRules", rules: [] }));
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "rulesNotShown", tooLarge: 0, listed: true }));
                probeWindow.check(placeholder && placeholder.visible
                                  && placeholder.text === "No rules yet", "no placeholder: "
                                  + (placeholder ? placeholder.visible + " " + placeholder.text
                                                 : "missing"));
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "rulesNotShown", tooLarge: 0, overLimitTotal: 12000,
                    listed: false }));
                probeWindow.check(!placeholder.visible, "the placeholder says no rules");
                // With no list from the firewall service: waiting, not "no rules".
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "rulesNotShown", tooLarge: 0, listed: false }));
                probeWindow.check(placeholder.visible
                                  && placeholder.text === "Waiting for the firewall service's rules",
                                  "waiting placeholder: " + placeholder.text);
                // A withdrawal's empty list, before its RulesNotShown: no flash (N8).
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "setRules", rules: [] }));
                probeWindow.check(placeholder.text === "Waiting for the firewall service's rules",
                                  "no 'No rules yet' flash: " + placeholder.text);

                if (probeWindow.failures.length > 0) {
                    throw new Error("all-apps probe: " + probeWindow.failures.join("; "));
                }
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
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RulesPage all-apps probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage all-apps probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
