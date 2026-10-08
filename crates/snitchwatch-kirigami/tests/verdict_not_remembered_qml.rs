//! Issue #44, second half, GUI side. The bridge never remembers an answer for
//! a program it can't identify by an absolute file path, so:
//!   * the decision sheet offers only "This time" for such a row (and submits
//!     only "this_time", whatever its selector holds), with the bridge's own
//!     explanation as a hint — while an absolute path keeps every duration;
//!   * a `verdictNotRemembered` from the bridge becomes a passive notification
//!     with that same fixed sentence;
//!   * both QML copies of the sentence stay equal to
//!     `RuleRefusal::describe`.
//!
//! Same probe shape as `stuck_pending_inspector_qml.rs`: a real
//! `ConnectionsPage` + `ConnectionsModel` driven with bridge messages, failures
//! collected and thrown against the probe URL. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::translator::process_binding::RuleRefusal;

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/verdict_not_remembered_probe.qml";

#[test]
fn qml_explanations_match_the_bridges_refusal_sentence() {
    let quoted = format!("\"{}\"", RuleRefusal::ProcessFileUnknown.describe());
    for (name, source) in [
        (
            "PendingDecisionSheet.qml",
            include_str!("../qml/PendingDecisionSheet.qml"),
        ),
        (
            "InlineVerdicts.qml",
            include_str!("../qml/InlineVerdicts.qml"),
        ),
    ] {
        assert!(
            source.contains(&quoted),
            "{name} no longer shows the bridge's refusal sentence verbatim: {quoted}"
        );
    }
}

#[test]
fn only_a_bindable_program_is_offered_a_remembered_answer() {
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

    readonly property string sentence: "Snitchwatch couldn't identify this program's file, so this answer applies only to this connection."
    property var shown: []
    property var submitted: []
    property var failures: []

    // Kirigami.ApplicationWindow's API, recorded instead of drawn.
    function showPassiveNotification(message, timeout) {
        probeWindow.shown.push(message);
    }

    QtObject {
        id: feedStub
        property bool ok: true
        signal verdictNotRemembered(string rowId)
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {
            probeWindow.submitted.push({ rowId: rowId, duration: duration });
        }
        // A bridge with app-bound rules, so the sheet's durations depend on
        // the program alone (issue #72 covers older bridges).
        function appBoundRulesFor(rowId) {
            return true;
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
    function lastDuration() {
        const last = probeWindow.submitted[probeWindow.submitted.length - 1];
        return last ? last.duration : "(nothing submitted)";
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
                        probeWindow.row("kernel", "Kernel connection"),
                        probeWindow.row("comm", "curl"),
                        probeWindow.row("relative", "bin/curl"),
                        probeWindow.row("none", null)
                    ]
                }));
                const sheet = page.decisionSheet;
                const durations = sheet.durationSelector;

                probeWindow.openOn("abs");
                probeWindow.check(sheet.bindableProcessPath === true,
                                  "an absolute path is not bindable");
                probeWindow.check(durations.count === 4,
                                  "an absolute path offers " + durations.count + " durations, not 4");
                durations.currentIndex = 3;
                sheet.submit("allow");
                probeWindow.check(probeWindow.lastDuration() === "forever",
                                  "an absolute path could not be remembered: " + probeWindow.lastDuration());

                for (const id of ["kernel", "comm", "relative", "none"]) {
                    probeWindow.openOn(id);
                    probeWindow.check(sheet.bindableProcessPath === false, id + ": bindable");
                    probeWindow.check(durations.count === 1 && durations.currentValue === "this_time",
                                      id + ": offers " + durations.count + " durations");
                    sheet.submit("allow");
                    probeWindow.check(probeWindow.lastDuration() === "this_time",
                                      id + ": submitted " + probeWindow.lastDuration());
                }

                // Whatever the selector holds, a non-bindable row submits once.
                probeWindow.openOn("kernel");
                durations.model = [{ label: "Forever", token: "forever" }];
                durations.currentIndex = 0;
                sheet.submit("allow");
                probeWindow.check(probeWindow.lastDuration() === "this_time",
                                  "submit() trusted the selector: " + probeWindow.lastDuration());

                feedStub.verdictNotRemembered("1:ask-1");
                probeWindow.check(probeWindow.shown.length === 1
                                  && probeWindow.shown[0] === probeWindow.sentence,
                                  "passive notification: " + JSON.stringify(probeWindow.shown));

                if (probeWindow.failures.length > 0) {
                    throw new Error("verdict-not-remembered probe: "
                                    + probeWindow.failures.join("; "));
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
        "verdict-not-remembered probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "verdict-not-remembered probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
