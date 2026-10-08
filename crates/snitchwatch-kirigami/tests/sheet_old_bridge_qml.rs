//! Issue #72: on a bridge session that didn't advertise app-bound rules, the
//! decision sheet must not remember a "This host only" or "Any host on this
//! domain" answer. Such a bridge stores those rules for every app. It offers
//! only "This time" for them, says why, and `submit()` sends `this_time`
//! whatever state the selector is left in (pre-selected, switched scope,
//! forced index or model). "Any host" (the program alone) and capable bridges
//! keep every duration. `bridge_runtime/verdict_gate_tests.rs` enforces the
//! same rule again in Rust, under every QML path.
//!
//! Same probe shape as `inline_verdict_qml.rs`: a real `ConnectionsPage` and
//! `ConnectionsModel` with a stub feed. The stub's `appBound` stands for the
//! session's capability. Fails on any QML warning from the probe or the
//! shell's QML. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/sheet_old_bridge_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn an_old_bridge_never_gets_a_remembered_host_scoped_answer() {
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
    width: 800
    height: 600

    readonly property string note: "This firewall bridge is too old to limit a rule to just this program, so it can only answer this connection. Update Snitchwatch's background service to remember answers."
    property var failures: []
    property string last: ""

    QtObject {
        id: feedStub
        property bool appBound: false
        function submitVerdict(rowId, choice, scope, duration) {
            probeWindow.last = rowId + " " + scope + "/" + duration;
            return true;
        }
        function appBoundRulesFor(rowId) {
            return feedStub.appBound;
        }
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
    function row(id, processPath) {
        return { id: id, process: "p", processPath: processPath, dstHost: "example.com",
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                 matchedRule: null };
    }
    function openOn(id) {
        page.openInspector({
            rowId: id, process: "p", host: "example.com", port: 443,
            protocol: "tcp", verdict: "pending", pending: true,
            matchedRule: "", matchedRuleDisplay: ""
        });
    }
    // Submits with the sheet as it is and checks what was sent.
    function expectSubmit(expected, what) {
        probeWindow.last = "";
        page.decisionSheet.submit("allow");
        probeWindow.check(probeWindow.last === expected, what + ": sent " + probeWindow.last);
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                connModel.applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        probeWindow.row("abs", "/usr/bin/curl"),
                        probeWindow.row("abs2", "/usr/bin/wget"),
                        probeWindow.row("kernel", "Kernel connection")
                    ]
                }));
                const sheet = page.decisionSheet;
                const scopes = sheet.scopeSelector;
                const durations = sheet.durationSelector;

                // Default open on an old bridge: only "This time", and why.
                probeWindow.openOn("abs");
                probeWindow.check(scopes.currentValue === "this_host", "default scope");
                probeWindow.check(durations.count === 1 && durations.currentValue === "this_time",
                                  "this_host offers " + durations.count + " durations");
                probeWindow.check(sheet.showOldBridgeNote === true, "this_host: no note");
                probeWindow.check(sheet.oldBridgeNote.text === probeWindow.note
                                  && sheet.oldBridgeNote.textFormat === Text.PlainText,
                                  "note text " + sheet.oldBridgeNote.text);
                probeWindow.check(sheet.showDenyOnceHint === false, "this_host: Deny hint");
                probeWindow.expectSubmit("abs this_host/this_time", "this_host");

                scopes.currentIndex = 1;
                probeWindow.check(durations.count === 1 && sheet.showOldBridgeNote === true,
                                  "any_host_on_domain offers " + durations.count);
                probeWindow.expectSubmit("abs any_host_on_domain/this_time", "domain");

                // "Any host" matches the program alone: every duration.
                scopes.currentIndex = 2;
                probeWindow.check(durations.count === 4 && sheet.showOldBridgeNote === false,
                                  "any_host offers " + durations.count);
                probeWindow.check(sheet.showDenyOnceHint === true, "any_host: no Deny hint");
                durations.currentIndex = 3;
                probeWindow.expectSubmit("abs any_host/forever", "any_host");

                // Forever chosen, then the scope switched back.
                durations.currentIndex = 3;
                scopes.currentIndex = 0;
                probeWindow.expectSubmit("abs this_host/this_time", "scope switched after Forever");
                scopes.currentIndex = 2;
                durations.currentIndex = 3;
                scopes.currentIndex = 1;
                probeWindow.expectSubmit("abs any_host_on_domain/this_time",
                                         "domain switched after Forever");

                // Forever pre-selected on a capable bridge, then an old-bridge row.
                feedStub.appBound = true;
                probeWindow.openOn("abs");
                scopes.currentIndex = 0;
                probeWindow.check(durations.count === 4 && sheet.showOldBridgeNote === false,
                                  "capable this_host offers " + durations.count);
                durations.currentIndex = 3;
                probeWindow.expectSubmit("abs this_host/forever", "capable bridge");
                durations.currentIndex = 3;
                feedStub.appBound = false;
                probeWindow.openOn("abs2");
                probeWindow.expectSubmit("abs2 this_host/this_time", "pre-selected Forever");

                // The program comes first: the #44 note, not this one.
                probeWindow.openOn("kernel");
                probeWindow.check(sheet.bindableProcessPath === false
                                  && sheet.showOldBridgeNote === false,
                                  "kernel row shows the old-bridge note");

                // A stale index, then a forced model: still once.
                probeWindow.openOn("abs");
                scopes.currentIndex = 0;
                durations.currentIndex = 3;
                probeWindow.expectSubmit("abs this_host/this_time", "stale index");
                durations.model = [{ label: "Forever", token: "forever" }];
                durations.currentIndex = 0;
                probeWindow.expectSubmit("abs this_host/this_time", "forced selector");

                if (probeWindow.failures.length > 0) {
                    throw new Error("sheet old-bridge probe: " + probeWindow.failures.join("; "));
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
        "sheet old-bridge probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "sheet old-bridge probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
