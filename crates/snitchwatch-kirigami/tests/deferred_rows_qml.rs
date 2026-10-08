//! Prompt-slot plan Part C, GUI half. One probe drives a real
//! `ConnectionsPage`, `ConnectionsModel` and `PromptSlotBanner` with the
//! messages the bridge feed delivers, and a feed stub that says which bridge
//! sessions take "Decide later":
//!   * put-off rows (`deferred`) say what happened, as fixed plain text, in
//!     the flat and the grouped view, and never count as waiting;
//!   * a pending row's inspector counts down to `answerDeadlineMs`;
//!   * "Decide later" is offered on a pending row, in the inspector and on
//!     the banner only for a session that takes it, sends once per row, and
//!     the inspector says so plainly where it isn't offered;
//!   * "Make a rule…" appears for a put-off row whose program is known.
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

const PROBE_URL: &str = "qrc:/deferred_rows_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
const DONE_MARKER: &str = "DEFERRED_ROWS_PROBE_DONE";

#[test]
fn put_off_rows_countdown_decide_later_and_make_a_rule() {
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
    property int waits: 0

    function showPassiveNotification(message, timeout) {}

    // Session 1 takes "Decide later"; session 2 is an older bridge.
    QtObject {
        id: feedStub
        property bool ok: true
        property var later: []
        function submitVerdict(rowId, choice, scope, duration, bindable) { return true; }
        function appBoundRulesFor(rowId) { return true; }
        function decideLaterFor(rowId) { return rowId.startsWith("1:"); }
        function decideLater(rowId) {
            feedStub.later.push(rowId);
            return true;
        }
    }

    QtObject {
        id: statusStub
        property bool supported: true
        property bool held: false
        property string rowId: ""
        property string text: "holder"
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel {
            id: connModel
        }
    }

    PromptSlotBanner {
        id: banner
        status: statusStub
        model: connModel
        bridgeFeed: feedStub
        armDelayMs: 100
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
                    bytesSent: 0, bytesReceived: 0, startedAtMs: 0, matchedRule: null };
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

    readonly property var labels: ({
        "1:wait": "pending",
        "2:old": "pending",
        "1:to-allow": "Not answered in time: usually allowed (the firewall's default action)",
        "1:to-deny": "Not answered in time: denied (the firewall's default action)",
        "1:to-unknown": "Not answered in time: the firewall's default action",
        "1:later-blocked": "Decided later: this program is blocked for 5 minutes",
        "1:later-default": "Decided later: the firewall's default action",
        "1:later-kernel": "Decided later: usually allowed (the firewall's default action)"
    })
    function checkLabels(view) {
        for (const id of Object.keys(probeWindow.labels)) {
            const item = probeWindow.rowDelegate(id);
            if (!item) {
                probeWindow.failures.push(view + ": no row delegate for " + id);
                continue;
            }
            const label = probeWindow.child(item, "verdictLabel");
            probeWindow.check(label !== null && label.text === probeWindow.labels[id]
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
                            probeWindow.row("1:wait", { answerDeadlineMs: Date.now() + 20000 }),
                            probeWindow.row("1:wait2", { answerDeadlineMs: Date.now() + 20000 }),
                            probeWindow.row("2:old", {}),
                            probeWindow.row("1:to-allow", { deferred: true, autoAnswer: "noAnswer",
                                                            action: "allow" }),
                            probeWindow.row("1:to-deny", { deferred: true, autoAnswer: "noAnswer",
                                                           action: "deny" }),
                            probeWindow.row("1:to-unknown", { deferred: true,
                                                              autoAnswer: "noAnswer" }),
                            probeWindow.row("1:later-blocked", { deferred: true, action: "deny",
                                                                 matchedRule: "deny-curl" }),
                            probeWindow.row("1:later-default", { deferred: true }),
                            probeWindow.row("1:later-kernel", { deferred: true, action: "allow",
                                                                processPath: "Kernel connection" })
                        ]
                    }));
                } else if (probeWindow.phase === 1) {
                    probeWindow.checkLabels("flat");
                    probeWindow.check(connModel.pendingCount === 3,
                                      "pending count " + connModel.pendingCount
                                      + ": put-off rows count as waiting");

                    // Decide later on a row: only for session 1, only while
                    // pending, sent once.
                    for (const id of Object.keys(probeWindow.labels)) {
                        const button = probeWindow.child(probeWindow.rowDelegate(id),
                                                         "decideLaterButton");
                        probeWindow.check(button !== null
                                          && button.visible === (id === "1:wait"),
                                          "Decide later on " + id + ": "
                                          + (button ? button.visible : "missing"));
                    }
                    const button = probeWindow.child(probeWindow.rowDelegate("1:wait"),
                                                     "decideLaterButton");
                    button.clicked();
                    button.clicked();
                    probeWindow.check(JSON.stringify(feedStub.later) === '["1:wait"]',
                                      "Decide later sent " + JSON.stringify(feedStub.later));

                    // The inspector of a pending row counts down and offers it.
                    page.openInspector(probeWindow.rowDelegate("1:wait"));
                    const left = page.decisionSheet.remainingSeconds;
                    probeWindow.check(left >= 18 && left <= 20, "countdown " + left);
                    probeWindow.check(page.decisionSheet.decideLater === true,
                                      "the sheet doesn't offer Decide later");
                    probeWindow.check(!page.makeRuleSheet.visible, "Make a rule on a pending row");

                    // An older bridge: no button, and no countdown to show.
                    page.openInspector(probeWindow.rowDelegate("2:old"));
                    probeWindow.check(page.decisionSheet.decideLater === false,
                                      "Decide later offered for an older bridge");
                    probeWindow.check(page.decisionSheet.remainingSeconds === -1,
                                      "a countdown without a deadline");

                    // A put-off row: its outcome, and Make a rule.
                    page.openInspector(probeWindow.rowDelegate("1:to-allow"));
                    probeWindow.check(page.inspectVerdictText === probeWindow.labels["1:to-allow"],
                                      "inspector verdict " + page.inspectVerdictText);
                    probeWindow.check(page.makeRuleSheet.visible
                                      && page.makeRuleSheet.openButton.visible,
                                      "no Make a rule for a put-off row");
                    page.makeRuleSheet.openButton.clicked();
                    probeWindow.check(page.makeRuleSheet.form.visible, "the rule form didn't open");
                    // No bridge runs here, so nothing can be sent.
                    page.makeRuleSheet.make("deny");
                    probeWindow.check(page.makeRuleSheet.result === "The rule couldn't be sent.",
                                      "make result " + page.makeRuleSheet.result);
                    page.openInspector(probeWindow.rowDelegate("1:later-kernel"));
                    probeWindow.check(page.makeRuleSheet.visible
                                      && !page.makeRuleSheet.openButton.visible,
                                      "Make a rule offered for an unknown program");

                    // The banner, for a holder of each session.
                    statusStub.rowId = "1:wait2";
                    statusStub.held = true;
                } else if (probeWindow.phase === 2 && !banner.armed && probeWindow.waits++ < 20) {
                    // A busy previous phase can tick before the banner arms.
                    return;
                } else if (probeWindow.phase === 2) {
                    probeWindow.check(banner.decideLaterOffered, "the banner doesn't offer it");
                    probeWindow.check(banner.actionable, "banner not actionable: shown "
                                      + banner.shown + " armed " + banner.armed + " pending "
                                      + connModel.isPendingRow("1:wait2") + " answered "
                                      + banner.answeredRowId);
                    banner.putOff();
                    banner.putOff();
                    probeWindow.check(JSON.stringify(feedStub.later) === '["1:wait","1:wait2"]',
                                      "banner sent " + JSON.stringify(feedStub.later));
                    statusStub.rowId = "2:old";
                    probeWindow.check(!banner.decideLaterOffered,
                                      "the banner offers it for an older bridge");
                    connModel.setGroupedMode(true);
                } else if (probeWindow.phase === 3) {
                    for (const d of probeWindow.delegates()) {
                        if (d.isGroupHeader && d.depth === 0 && !d.expanded) {
                            connModel.toggleProcessGroup(d.groupKey);
                        }
                    }
                } else if (probeWindow.phase === 4) {
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
                        throw new Error("deferred rows probe: " + probeWindow.failures.join("; "));
                    }
                    console.warn("DEFERRED_ROWS_PROBE_DONE");
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
        "deferred rows probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "the probe never finished; stderr:\n{captured}"
    );
}
