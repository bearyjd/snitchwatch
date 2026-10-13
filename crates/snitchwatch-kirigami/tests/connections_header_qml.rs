//! The nested page header (filter bar + rule actions, PR #143) on a REAL
//! `ConnectionsPage`, next to the inspector side panel:
//!   * `page.levelRules.create(0, ...)` opens the program-wide confirm dialog
//!     through the page (nothing else drove the nested header);
//!   * while a rule message is showing the header grows, and the first list row
//!     still starts at or below the header's bottom edge (the message pushes
//!     the list down instead of overlapping it).
//!
//! One probe, one step per timer tick. Headless: `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_header_probe.qml";
const DONE_MARKER: &str = "CONNECTIONS_HEADER_PROBE_DONE";

#[test]
fn the_nested_header_opens_the_confirm_and_keeps_the_list_below_it() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1200
    height: 700
    property var failures: []
    property int step: 0

    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: ConnectionsModel { id: model }
    }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function headerBottom() {
        return page.header.mapToItem(page, 0, page.header.height).y;
    }
    function firstRowTop() {
        const d = page.connectionList.itemAtIndex(0);
        return d ? d.mapToItem(page, 0, 0).y : -1;
    }

    readonly property var steps: [
        function () {
            model.setGroupedMode(false);
            model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                { id: "a", process: "p", processPath: "/usr/bin/p", dstHost: "a.example.com",
                  dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 }] }));
            page.levelRules.create(0, "/usr/bin/p", "", "p", "deny");
            probe.expect(page.levelRules.confirmDialog.visible,
                         "program level did not open the confirm through the page");
            page.levelRules.confirmDialog.close();
        },
        function () {
            probe.expect(!page.levelRules.visible, "rule bar visible before any message");
            probe.expect(probe.firstRowTop() >= probe.headerBottom(),
                         "row above header bottom without a message: " + probe.firstRowTop()
                         + " < " + probe.headerBottom());
            probe.expect(page.header.height > 0, "header has no height");
            // Without a bridge a domain-level command is refused with a message.
            page.levelRules.create(1, "/usr/bin/p", "example.com", "example.com", "allow");
            probe.expect(page.levelRules.message !== "", "no rule message after a refused command");
        },
        function () {
            probe.expect(page.levelRules.visible, "rule message not shown");
            probe.expect(page.levelRules.height > 0, "rule message has no height");
            const bottom = probe.headerBottom();
            probe.expect(bottom >= page.levelRules.mapToItem(page, 0, page.levelRules.height).y,
                         "header does not contain the rule message");
            probe.expect(probe.firstRowTop() >= bottom,
                         "first row starts above the header bottom while a message shows: "
                         + probe.firstRowTop() + " < " + bottom);
        }
    ]

    Timer {
        interval: 150
        running: true
        repeat: true
        onTriggered: {
            if (probe.step >= probe.steps.length) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("connections header probe: " + probe.failures.join("; "));
                    }
                    console.warn("CONNECTIONS_HEADER_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
                return;
            }
            try {
                probe.steps[probe.step]();
            } catch (e) {
                probe.failures.push("step " + probe.step + " threw: " + e);
            }
            probe.step++;
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
        "probe root was null:\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "connections header probe failed:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
