//! The Security Scan page must show the report the host broker returns.
//! The report below is the real output of a deep scan on the Snitchwatch system
//! image, so this proves the page draws what the scanner actually emits.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::scanner_controller as _;
mod common;
use common::{capture_stderr, init_headless_qt_env};

const REPORT: &str = r#"{
  "informational": [
    {"detail": "Secure Boot is currently enabled", "path": "lockdown:secureboot"},
    {"detail": "kernel lockdown is currently 'none'", "path": "lockdown:kernel"}
  ],
  "new": [],
  "resolved": [],
  "scan_id": 5,
  "skipped": [
    {"check": "file_drift", "reason": "delegated to shared baseline cache (baseline.db); lands when Phase 5 merges"},
    {"check": "rootkit_signature", "reason": "chkrootkit not installed on this host"}
  ],
  "still_outstanding": []
}"#;

#[test]
fn a_returned_report_is_drawn_on_the_scan_page() {
    init_headless_qt_env();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = format!(
        r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {{
    visible: true
    width: 900; height: 700
    ScannerController {{ id: controller }}
    property var page: null
    property int ticks: 0

    // Every item below `root` once (a contentItem can also be a child), through
    // ordinary children and Flickable content.
    function walk(root, out, seenItems) {{
        if (!root || seenItems.has(root)) return out;
        seenItems.add(root);
        out.push(root);
        const kids = [];
        if (root.children) for (let i = 0; i < root.children.length; ++i) kids.push(root.children[i]);
        if (root.contentItem && root.contentItem !== root) kids.push(root.contentItem);
        for (const k of kids) walk(k, out, seenItems);
        return out;
    }}
    // `visible` is already effective (false when any ancestor is hidden); also
    // require real size, so a laid-out-but-collapsed item does not count as drawn.
    function drawn(i) {{ return i.visible && i.width > 0 && i.height > 0; }}
    function texts() {{
        return walk(page, [], new Set())
            .filter(i => i.objectName === "scannerFindingPath" && drawn(i)).map(i => i.text);
    }}
    function headings() {{
        return walk(page, [], new Set())
            .filter(i => typeof i.level === "number" && typeof i.text === "string" && drawn(i))
            .map(i => i.text);
    }}
    function report(state, detail) {{
        console.warn("SCANNER_RENDER_RESULT state=" + state + " " + detail);
        Qt.quit();
    }}

    Component.onCompleted: {{
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/ScannerPage.qml");
        if (component.status !== Component.Ready) throw new Error(component.errorString());
        page = component.createObject(contentItem, {{ controller: controller, width: 900, height: 700 }});
        controller.reportJson = {report_literal};
    }}
    Timer {{
        interval: 100; running: true; repeat: true
        onTriggered: {{
            ticks += 1;
            try {{
                const t = texts(), h = headings();
                if (t.length === 4) {{
                    report("ready", "findings=" + JSON.stringify(t) + " headings=" + JSON.stringify(h));
                }}
            }} catch (e) {{
                report("error", String(e));
            }}
        }}
    }}
    // Independent of the polling above, so nothing can leave the event loop running.
    Timer {{
        interval: 8000; running: true
        onTriggered: report("timeout", "findings=" + JSON.stringify(texts()) + " headings=" + JSON.stringify(headings()))
    }}
}}
"#,
        report_literal = serde_json::to_string(REPORT).expect("quote the report")
    );
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_, obj, _| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(qml.as_str()),
                &QUrl::from("qrc:/inline_scanner_render_probe.qml"),
            );
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
        "page failed to load: {captured}"
    );
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    let line = captured
        .lines()
        .find(|l| l.contains("SCANNER_RENDER_RESULT"))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    assert!(
        line.contains("state=ready"),
        "report was not drawn in time: {line}"
    );
    for expected in [
        "Secure Boot is currently enabled",
        "kernel lockdown is currently",
        "file_drift",
        "rootkit_signature",
        "Informational (2)",
        "Skipped / unavailable checks (2)",
        "New anomalies (0)",
    ] {
        assert!(line.contains(expected), "missing {expected:?} in: {line}");
    }
}
