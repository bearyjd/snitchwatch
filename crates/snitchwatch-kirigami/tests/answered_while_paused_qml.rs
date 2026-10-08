//! Issue #78, GUI half: a row the bridge allowed once because filtering was
//! paused (`autoAnswer: "filterPaused"`) says so in the Connections list, as
//! fixed plain text, in the flat view and in the grouped view's leaf rows.
//! That holds for a row inserted already answered (an Ask during the pause)
//! and for a waiting row the pause answered later, which arrives as an update
//! (flat mode: only `dataChanged`). Other rows keep their usual label, and a
//! row carrying a reason only a newer bridge knows still loads, with the
//! usual label.
//!
//! The probe drives a real `ConnectionsPage` and `ConnectionsModel` with the
//! messages the bridge feed delivers and reads each row delegate's own
//! verdict label. Failures are collected and thrown together against the
//! probe's URL, which the stderr capture turns into an assertion; the same
//! capture catches a delegate whose required role the model doesn't supply.
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/answered_while_paused_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
const DONE_MARKER: &str = "ANSWERED_WHILE_PAUSED_PROBE_DONE";

#[test]
fn a_row_allowed_while_paused_says_so_in_flat_and_grouped_views() {
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
    height: 900

    property var failures: []
    property int phase: 0

    // Stand-in for BridgeFeed: the page only reads these.
    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindable) { return true; }
        function appBoundRulesFor(rowId) { return true; }
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
    function row(id, host, action, autoAnswer) {
        const r = { id: id, process: "curl", processPath: null, dstHost: host,
                    dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                    action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                    matchedRule: null };
        if (autoAnswer !== undefined) {
            r.autoAnswer = autoAnswer;
        }
        return r;
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
    function verdictLabel(item) {
        if (item.objectName === "verdictLabel") {
            return item;
        }
        for (let i = 0; i < item.children.length; i++) {
            const found = probeWindow.verdictLabel(item.children[i]);
            if (found) {
                return found;
            }
        }
        return null;
    }
    function checkLabels(view) {
        const expected = {
            paused: "Allowed once (filtering was paused)",
            answeredLater: "Allowed once (filtering was paused)",
            other: "allowed",
            waiting: "pending",
            newer: "allowed"
        };
        const shown = probeWindow.delegates();
        for (const id of Object.keys(expected)) {
            const item = shown.find(d => !d.isGroupHeader && d.rowId === id);
            if (!item) {
                probeWindow.failures.push(view + ": no row delegate for " + id);
                continue;
            }
            const label = probeWindow.verdictLabel(item);
            if (!label) {
                probeWindow.failures.push(view + ": " + id + " has no verdict label");
                continue;
            }
            probeWindow.check(label.text === expected[id],
                              view + ": " + id + " says '" + label.text + "', expected '"
                              + expected[id] + "'");
            probeWindow.check(label.textFormat === Text.PlainText,
                              view + ": " + id + "'s verdict label is not plain text");
        }
    }

    Timer {
        interval: 50
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
                            probeWindow.row("paused", "a.example.com", "allow", "filterPaused"),
                            probeWindow.row("other", "b.example.com", "allow"),
                            probeWindow.row("waiting", "c.example.com", null),
                            probeWindow.row("newer", "d.example.com", "allow",
                                            "aReasonFromANewerBridge"),
                            probeWindow.row("answeredLater", "e.example.com", null)
                        ]
                    }));
                } else if (probeWindow.phase === 1) {
                    // The pause answers the waiting row: the bridge sends it
                    // again, decided and labelled.
                    connModel.applyServerMessageJson(JSON.stringify({
                        action: "updateConnectionRows",
                        rows: [probeWindow.row("answeredLater", "e.example.com", "allow",
                                               "filterPaused")]
                    }));
                } else if (probeWindow.phase === 2) {
                    probeWindow.checkLabels("flat");
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
                        throw new Error("answered-while-paused probe: "
                                        + probeWindow.failures.join("; "));
                    }
                    console.warn("ANSWERED_WHILE_PAUSED_PROBE_DONE");
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
        "probe root was null: a QML parse error in the probe or ConnectionsPage.qml"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "answered-while-paused probe failed:\n{}",
        bad_lines.join("\n")
    );
    // The probe ran to the end: an empty capture is not a pass.
    assert!(
        captured.contains(DONE_MARKER),
        "the probe never finished; stderr:\n{captured}"
    );
}
