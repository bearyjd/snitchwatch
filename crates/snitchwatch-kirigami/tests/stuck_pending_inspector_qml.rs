//! Integration smoke: issue #49's stuck pending rows, GUI half. The inspector
//! must stop offering Allow/Deny once the pending row it was opened on is gone.
//!
//! The bridge now removes a pending row when opensnitchd's `AskRule` deadline
//! cancels it (see `tests/bridge_protocol_test.rs`,
//! `ask_rule_deadline_removes_row_for_silent_gui_and_rejects_late_verdict`),
//! but `ConnectionsPage.qml` copies the row into page properties when the
//! inspector opens and used to never look again. A late Allow click was then
//! rejected by the bridge while the sheet closed as if it had worked.
//!
//! One probe runs every scenario in sequence, because only one
//! `QGuiApplication` can exist per process. It drives a real
//! `ConnectionsPage` and a real `ConnectionsModel` with the same messages the
//! bridge feed delivers, and reads the page's own inspector state afterwards:
//!   * the default grouped view first, where with no bridge runtime every
//!     message lands behind a full model reset (`modelReset`);
//!   * then the flat, unfiltered view, where a removal is an incremental
//!     `rowsRemoved`, an insert is `rowsInserted` and an update is only
//!     `dataChanged`;
//!   * in both: a row removed, `ClearConnectionRows`, and a same-session
//!     snapshot, which clears and then re-inserts the very same still-pending
//!     row and must put the prompt back;
//!   * a session change: the old session's row is cleared and the new session
//!     reuses the same wire id, so only the qualified id differs, and the old
//!     prompt must stay withdrawn;
//!   * a row decided elsewhere, which flat mode reports only as `dataChanged`;
//!   * a lost bridge connection, in the order the app sees it: `ok` goes false
//!     while the model still holds the old session's pending row, and nothing
//!     short of that session's rows being replaced brings the prompt back;
//!   * controls: an unrelated removal or update, and re-opening the
//!     inspector, leave a live pending row decidable.
//!
//! Failures are collected and thrown together, so a red run names every
//! scenario that broke. The throw is reported by Qt against the probe's URL,
//! which the Rust side turns into an assertion (see `inline_verdict_qml.rs`).
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/stuck_pending_inspector_probe.qml";

#[test]
fn inspector_stops_offering_a_verdict_once_its_pending_row_is_gone() {
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

    // Stand-in for BridgeFeed: the page only needs `ok` and `submitVerdict`.
    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration) {}
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel {
            id: connModel
        }
    }

    property var failures: []
    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }

    function row(id, action) {
        return { id: id, process: "curl", processPath: null, dstHost: "example.com",
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                 matchedRule: null };
    }
    function send(msg) {
        connModel.applyServerMessageJson(JSON.stringify(msg));
    }
    function insertPending(id) {
        send({ action: "insertConnectionRows", rows: [probeWindow.row(id, null)] });
    }
    // `openInspector` takes the delegate's fields, not a model row.
    function openOn(id) {
        page.openInspector({
            rowId: id, process: "curl", host: "example.com", port: 443,
            protocol: "tcp", verdict: "pending", pending: true,
            matchedRule: "", matchedRuleDisplay: ""
        });
    }
    function stillOffered() {
        return page.inspectPending === true && page.inspectNoLongerPending !== true
            && page.inspectVerdictText === "pending";
    }
    function withdrawn() {
        return page.inspectPending === false && page.inspectNoLongerPending === true
            && page.inspectVerdictText === "no longer pending";
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                // --- Grouped mode (the default view) -------------------------
                // No bridge runtime here, so each message resets the model.
                probeWindow.insertPending("g1");
                probeWindow.openOn("g1");
                probeWindow.check(probeWindow.stillOffered(),
                                  "grouped: not offered right after opening on a pending row");
                probeWindow.send({ action: "clearConnectionRows" });
                probeWindow.check(probeWindow.withdrawn(),
                                  "grouped: still pending after ClearConnectionRows");
                // A same-session snapshot re-inserts the row that is still pending.
                probeWindow.insertPending("g1");
                probeWindow.check(probeWindow.stillOffered(),
                                  "grouped: a snapshot re-inserting the same pending row "
                                  + "did not restore the prompt");
                probeWindow.send({ action: "removeConnectionRows", ids: ["g1"] });
                probeWindow.check(probeWindow.withdrawn(),
                                  "grouped: still pending after its own row was removed");
                probeWindow.send({ action: "clearConnectionRows" });

                // --- Flat, unfiltered view -----------------------------------
                // Removals go through apply_remove and rowsRemoved, inserts
                // through rowsInserted, updates through dataChanged.
                connModel.setGroupedMode(false);

                // Removing an unrelated row leaves the open prompt alone;
                // removing its own row withdraws it.
                probeWindow.insertPending("r1");
                probeWindow.insertPending("r2");
                probeWindow.openOn("r1");
                probeWindow.check(probeWindow.stillOffered(),
                                  "remove: not offered right after opening on a pending row");
                probeWindow.send({ action: "removeConnectionRows", ids: ["r2"] });
                probeWindow.check(probeWindow.stillOffered(),
                                  "remove: an unrelated removal withdrew the prompt");
                probeWindow.send({ action: "removeConnectionRows", ids: ["r1"] });
                probeWindow.check(probeWindow.withdrawn(),
                                  "remove: still pending after its own row was removed");

                // Re-opening on a live pending row clears the notice.
                probeWindow.insertPending("r3");
                probeWindow.openOn("r3");
                probeWindow.check(probeWindow.stillOffered(),
                                  "reopen: notice from the previous row was not reset");

                // Clearing the model, then a same-session snapshot putting the
                // same still-pending row back (rowsInserted in flat mode).
                probeWindow.send({ action: "clearConnectionRows" });
                probeWindow.check(probeWindow.withdrawn(),
                                  "clear: still pending after ClearConnectionRows");
                probeWindow.insertPending("r3");
                probeWindow.check(probeWindow.stillOffered(),
                                  "snapshot: re-inserting the same pending row did not "
                                  + "restore the prompt");
                probeWindow.send({ action: "clearConnectionRows" });

                // Session change: the old session's row is cleared, then the new
                // session reuses wire id 7. The held id is "1:7"; "2:7" is a
                // different row and must not revive it.
                probeWindow.insertPending("1:7");
                probeWindow.openOn("1:7");
                probeWindow.check(probeWindow.stillOffered(),
                                  "session: not offered right after opening on 1:7");
                probeWindow.send({ action: "clearConnectionRows" });
                probeWindow.insertPending("2:7");
                probeWindow.check(probeWindow.withdrawn() && page.inspectId === "1:7",
                                  "session: prompt for 1:7 survived, or was revived by 2:7");
                probeWindow.send({ action: "clearConnectionRows" });

                // Decided elsewhere: flat mode reports an update as dataChanged
                // only. Another row being decided leaves the prompt alone.
                probeWindow.insertPending("d1");
                probeWindow.insertPending("d2");
                probeWindow.openOn("d1");
                probeWindow.send({ action: "updateConnectionRows",
                                   rows: [probeWindow.row("d2", "allow")] });
                probeWindow.check(probeWindow.stillOffered(),
                                  "decided: an unrelated update withdrew the prompt");
                probeWindow.send({ action: "updateConnectionRows",
                                   rows: [probeWindow.row("d1", "allow")] });
                probeWindow.check(probeWindow.withdrawn(),
                                  "decided: still pending after the row was decided elsewhere");
                probeWindow.send({ action: "clearConnectionRows" });

                // Lost connection, in the order the app sees it. `ok` goes false
                // while the model still holds the old session's pending row, so
                // the row check alone would keep offering a verdict that
                // can only fail with "disconnected".
                probeWindow.insertPending("1:7");
                probeWindow.openOn("1:7");
                probeWindow.check(probeWindow.stillOffered(),
                                  "disconnect: not offered right after opening on 1:7");
                feedStub.ok = false;
                probeWindow.check(probeWindow.withdrawn(),
                                  "disconnect: still offered while the connection is down");
                probeWindow.insertPending("1:8");
                probeWindow.check(probeWindow.withdrawn(),
                                  "disconnect: restored by an unrelated model event while down");
                // The connection coming back is not enough on its own: the new
                // session's snapshot replaces the old session's rows, and its
                // ids differ.
                feedStub.ok = true;
                probeWindow.check(probeWindow.withdrawn(),
                                  "reconnect: a status flip alone restored the prompt");
                probeWindow.send({ action: "clearConnectionRows" });
                probeWindow.insertPending("2:7");
                probeWindow.check(probeWindow.withdrawn() && page.inspectId === "1:7",
                                  "reconnect: the old prompt was revived by the new session");

                if (probeWindow.failures.length > 0) {
                    throw new Error(probeWindow.failures.join("; "));
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
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "stuck-pending inspector probe failed: root object was null — a QML parse error \
         (syntax error, or a type/property missing from ConnectionsPage.qml)."
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "the inspector kept offering a verdict for a pending row that no longer exists (or the \
         probe hit a QML runtime error). Each failed scenario is named below. Captured \
         stderr:\n{}",
        bad_lines.join("\n")
    );
}
