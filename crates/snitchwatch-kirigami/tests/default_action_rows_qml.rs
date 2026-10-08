//! E3 (plan `docs/superpowers/plans/2026-10-08-default-applied-events.md`),
//! GUI half. One probe drives a real `ConnectionsPage` and `ConnectionsModel`
//! with the rows a bridge sends for connections the firewall's default action
//! decided (`decidedByDefault`, no `matchedRule`), beside a rule row and an
//! unmarked row named "":
//!   * the list's verdict label names the firewall's default action and what
//!     it did, in the flat and the grouped view, and never counts as waiting;
//!   * the inspector's Verdict and Matched rule say the same, never a blank
//!     rule name;
//!   * no rule action that assumes a named rule ("Show rule") is offered for
//!     such a row, but "Make a rule…" is, as for a put-off row; a
//!     rule-matched row still gets none.
//!
//! Failures are collected and thrown against the probe's URL, which the
//! stderr capture turns into an assertion; it also catches a delegate whose
//! required role the model doesn't supply. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/default_action_rows_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
const DONE_MARKER: &str = "DEFAULT_ACTION_ROWS_PROBE_DONE";

#[test]
fn default_decided_rows_name_the_default_action_and_offer_make_a_rule_only() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 1000
    height: 1400

    property var failures: []
    property int phase: 0

    function showPassiveNotification(message, timeout) {}

    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindable) { return true; }
        function appBoundRulesFor(rowId) { return true; }
        function decideLaterFor(rowId) { return true; }
        function decideLater(rowId) { return true; }
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel {
            id: connModel
        }
    }

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function row(id, fields) {
        const r = { id: id, process: "curl", processPath: "/usr/bin/curl",
                    dstHost: id.replace(":", "-") + ".example.com", dstIp: "93.184.216.34",
                    dstPort: 443, protocol: "tcp", direction: "outgoing", action: null,
                    bytesSent: 0, bytesReceived: 0, startedAtMs: 0 };
        return Object.assign(r, fields);
    }
    function delegates() {
        const list = page.connectionList;
        list.forceLayout();
        const found = [];
        for (let i = 0; i < list.count; i++) {
            const item = list.itemAtIndex(i);
            if (item) {
                found.push(item);
            }
        }
        return found;
    }
    function rowDelegate(id) {
        return probeWindow.delegates().find(d => !d.isGroupHeader && d.rowId === id);
    }
    function child(item, name) {
        if (item.objectName === name) {
            return item;
        }
        for (let i = 0; i < item.children.length; i++) {
            const found = probeWindow.child(item.children[i], name);
            if (found) {
                return found;
            }
        }
        return null;
    }

    // Row id -> [list label, inspector Matched rule, "Show rule" target,
    // "Make a rule…" offered].
    readonly property var expected: ({
        "1:default-deny": ["Denied (the firewall's default action)",
                           "No rule: the firewall's default action (deny)", "", true],
        "1:default-allow": ["Usually allowed (the firewall's default action)",
                            "No rule: the firewall's default action (allow)", "", true],
        // The flag wins over a stray rule name: no "Show rule", no block note.
        "1:default-stray": ["Denied (the firewall's default action)",
                            "No rule: the firewall's default action (deny)", "", true],
        "1:rule": ["allowed", "899-curl-allow", "899-curl-allow", false],
        "1:named-empty": ["denied", "default action", "", false]
    })
    function checkLabels(view) {
        for (const id of Object.keys(probeWindow.expected)) {
            const item = probeWindow.rowDelegate(id);
            if (!item) {
                probeWindow.failures.push(view + ": no row delegate for " + id);
                continue;
            }
            const label = probeWindow.child(item, "verdictLabel");
            probeWindow.check(label !== null && label.text === probeWindow.expected[id][0]
                              && label.textFormat === Text.PlainText,
                              view + ": " + id + " says '" + (label ? label.text : "?") + "'");
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probeWindow.phase === 0) {
                    connModel.setGroupedMode(false);
                    connModel.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [
                            probeWindow.row("1:default-deny", { action: "deny",
                                                                decidedByDefault: true }),
                            probeWindow.row("1:default-allow", { action: "allow",
                                                                 decidedByDefault: true }),
                            probeWindow.row("1:default-stray", { action: "deny",
                                                                 decidedByDefault: true,
                                                                 matchedRule: "stray" }),
                            probeWindow.row("1:rule", { action: "allow",
                                                        matchedRule: "899-curl-allow" }),
                            // An older bridge, or a stock rule file named "".
                            probeWindow.row("1:named-empty", { action: "deny", matchedRule: "" })
                        ]
                    }));
                } else if (probeWindow.phase === 1) {
                    probeWindow.checkLabels("flat");
                    probeWindow.check(connModel.pendingCount === 0,
                                      "pending count " + connModel.pendingCount);
                    for (const id of Object.keys(probeWindow.expected)) {
                        const want = probeWindow.expected[id];
                        page.openInspector(probeWindow.rowDelegate(id));
                        probeWindow.check(page.inspectVerdictText === want[0],
                                          id + ": inspector verdict " + page.inspectVerdictText);
                        probeWindow.check(page.inspectMatchedRuleDisplay === want[1],
                                          id + ": matched rule " + page.inspectMatchedRuleDisplay);
                        // "Show rule" is shown for, and jumps to, this name only.
                        probeWindow.check(page.inspectMatchedRule === want[2],
                                          id + ": Show rule for '" + page.inspectMatchedRule + "'");
                        probeWindow.check(!page.inspectPending && !page.decisionSheet.visible,
                                          id + ": offered a decision");
                        // The two-rows hint is for the put-off twin, not these.
                        probeWindow.check(!page.makeRuleSheet.alsoListedNote.visible, id + ": two-rows hint");
                        probeWindow.check(page.makeRuleSheet.visible === want[3]
                                          && page.makeRuleSheet.openButton.visible === want[3],
                                          id + ": Make a rule offered " + page.makeRuleSheet.visible);
                    }
                    // A default-decided row has no 5-minute block to warn about,
                    // even with a stray rule name.
                    page.openInspector(probeWindow.rowDelegate("1:default-stray"));
                    page.makeRuleSheet.openButton.clicked();
                    probeWindow.check(page.makeRuleSheet.form.visible, "the rule form didn't open");
                    probeWindow.check(!page.makeRuleSheet.blockNote.visible,
                                      "the block note on a default-decided row");

                    // M1: only the bridge's result says the rule exists. No
                    // bridge runs here, so nothing can be sent.
                    const sheet = page.makeRuleSheet;
                    sheet.make("deny");
                    probeWindow.check(sheet.result === "The rule couldn't be sent."
                                      && !sheet.controller.created,
                                      "unsent: " + sheet.result);
                    // A request the bridge hasn't answered yet.
                    const pendingId = sheet.controller.begin(page.inspectId);
                    probeWindow.check(pendingId.length > 0, "no request id");
                    probeWindow.check(sheet.result === "Sending the rule to the firewall…"
                                      && !sheet.controller.created,
                                      "before the result: " + sheet.result);
                    probeWindow.check(sheet.controller.begin(page.inspectId) === "",
                                      "a second request while one waits");
                    sheet.controller.applyServerMessageJson(JSON.stringify({
                        action: "ruleCommandResult", requestId: pendingId,
                        outcome: { status: "rejected", reason: "a rule with this name exists" }
                    }));
                    probeWindow.check(sheet.result
                                      === "The rule wasn't created: a rule with this name exists"
                                      && !sheet.controller.created,
                                      "refused: " + sheet.result);
                    const okId = sheet.controller.begin(page.inspectId);
                    sheet.controller.applyServerMessageJson(JSON.stringify({
                        action: "ruleCommandResult", requestId: okId, outcome: { status: "ok" }
                    }));
                    probeWindow.check(sheet.result === "The rule was created."
                                      && sheet.controller.created,
                                      "ok: " + sheet.result);
                    // Another row's request says nothing here.
                    sheet.controller.begin("1:rule");
                    probeWindow.check(sheet.result === "", "another row's status: " + sheet.result);
                    connModel.setGroupedMode(true);
                } else if (probeWindow.phase === 2) {
                    for (const d of probeWindow.delegates()) {
                        if (d.isGroupHeader && d.depth === 0 && !d.expanded) {
                            connModel.toggleProcessGroup(d.groupKey);
                        }
                    }
                } else if (probeWindow.phase === 3) {
                    for (const d of probeWindow.delegates()) {
                        if (d.isGroupHeader && d.depth === 1 && !d.expanded) {
                            connModel.toggleDomainGroup(d.groupParentKey, d.groupKey);
                        }
                    }
                } else {
                    probeWindow.checkLabels("grouped");
                    done = true;
                }
                probeWindow.phase++;
            } catch (e) {
                probeWindow.failures.push("phase " + probeWindow.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probeWindow.failures.length > 0) {
                        throw new Error("default action rows probe: "
                                        + probeWindow.failures.join("; "));
                    }
                    console.warn("DEFAULT_ACTION_ROWS_PROBE_DONE");
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
        "probe root was null: a QML parse error in the probe or a shell file"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "default action rows probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "the probe never finished; stderr:\n{captured}"
    );
}
