//! The Security Scan page must show the report the host broker returns.
//! The report below is the real output of a deep scan on the Snitchwatch system
//! image, so this proves the page draws what the scanner actually emits.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_kirigami::scanner::{summarize, ScanError};

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

struct Scenario {
    report: &'static str,
    result: Result<String, ScanError>,
    finding_count: usize,
}

const FINDINGS: &str = r#"{"scan_id":6,"new":[{"path":"/x","detail":"d"}],"still_outstanding":[],
    "resolved":[],"informational":[],"skipped":[]}"#;

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            report: REPORT,
            result: Ok(REPORT.into()),
            finding_count: 4,
        },
        Scenario {
            report: FINDINGS,
            result: Ok(FINDINGS.into()),
            finding_count: 1,
        },
        Scenario {
            report: "",
            result: Err(ScanError {
                title: "Host scanner setup is required.",
                detail: "x".into(),
            }),
            finding_count: 0,
        },
    ]
}

/// One page per scenario in a single app (Qt allows one application per process); each
/// scenario's controller carries the outcome `summarize` derives. One result line each.
fn probe(scenarios: &[Scenario]) -> Vec<String> {
    init_headless_qt_env();
    let lit = |s: &str| serde_json::to_string(s).expect("quote");
    let specs: Vec<String> = scenarios
        .iter()
        .map(|s| {
            let (kind, summary) = summarize(&s.result, "09:05");
            let title = s.result.as_ref().err().map_or("", |e| e.title);
            format!(
                "{{report: {}, summary: {}, kind: {}, error: {}, count: {}}}",
                lit(s.report),
                lit(&summary),
                lit(kind.as_str()),
                lit(title),
                s.finding_count
            )
        })
        .collect();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = QML.replace("__SPECS__", &specs.join(","));
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
    let lines: Vec<String> = captured
        .lines()
        .filter(|l| l.contains("SCANNER_RENDER_RESULT"))
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), scenarios.len(), "probe incomplete: {captured}");
    lines
}

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    visible: true
    width: 900; height: 700
    property var specs: [__SPECS__]
    property var pages: []
    property var done: []
    property int ticks: 0

    Component { id: controllerComponent; ScannerController {} }

    // Every item below `root` once (a contentItem can also be a child).
    function walk(root, out, seenItems) {
        if (!root || seenItems.has(root)) return out;
        seenItems.add(root);
        out.push(root);
        const kids = [];
        if (root.children) for (let i = 0; i < root.children.length; ++i) kids.push(root.children[i]);
        if (root.contentItem && root.contentItem !== root) kids.push(root.contentItem);
        for (const k of kids) walk(k, out, seenItems);
        return out;
    }
    // `visible` is already effective; also require real size so a collapsed item does not count.
    function drawn(i) { return i.visible && i.width > 0 && i.height > 0; }
    function texts(page) {
        return walk(page, [], new Set())
            .filter(i => i.objectName === "scannerFindingPath" && drawn(i)).map(i => i.text);
    }
    function headingItems(page) {
        return walk(page, [], new Set())
            .filter(i => typeof i.level === "number" && typeof i.text === "string" && drawn(i));
    }
    function summaryItem(page) {
        return walk(page, [], new Set()).find(i => i.objectName === "scannerSummary" && drawn(i));
    }
    function finish(idx, state, detail) {
        if (done[idx]) return;
        done[idx] = true;
        console.warn("SCANNER_RENDER_RESULT idx=" + idx + " state=" + state + " " + detail);
        if (done.filter(d => d).length === specs.length) Qt.quit();
    }
    function snapshot(idx) {
        const page = pages[idx];
        const s = summaryItem(page), h = headingItems(page);
        const above = s && h.length > 0 && s.mapToItem(null, 0, 0).y < h[0].mapToItem(null, 0, 0).y;
        return "summary=" + JSON.stringify(s ? s.text : null) + " above=" + above
            + " findings=" + JSON.stringify(texts(page)) + " headings=" + JSON.stringify(h.map(i => i.text));
    }

    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/ScannerPage.qml");
        if (component.status !== Component.Ready) throw new Error(component.errorString());
        for (let i = 0; i < specs.length; ++i) {
            const sp = specs[i];
            const c = controllerComponent.createObject(contentItem);
            const page = component.createObject(contentItem, { controller: c, width: 900, height: 700 });
            pages.push(page);
            if (sp.report !== "") c.reportJson = sp.report;
            c.summaryText = sp.summary;
            c.summaryKind = sp.kind;
            c.errorTitle = sp.error;
            c.errorText = sp.error === "" ? "" : "detail";
        }
    }
    // Bounded polling (readiness gate): each page is ready once its summary shows the expected
    // text and its findings are drawn.
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            ticks += 1;
            try {
                for (let i = 0; i < specs.length; ++i) {
                    const s = summaryItem(pages[i]);
                    if (s && s.text === specs[i].summary && texts(pages[i]).length === specs[i].count)
                        finish(i, "ready", snapshot(i));
                }
            } catch (e) {
                for (let i = 0; i < specs.length; ++i) finish(i, "error", String(e));
            }
        }
    }
    // Independent of the polling above, so nothing can leave the event loop running.
    Timer {
        interval: 8000; running: true
        onTriggered: { for (let i = 0; i < specs.length; ++i) finish(i, "timeout", snapshot(i)); }
    }
}
"#;

#[test]
fn every_scan_outcome_is_stated_above_the_sections() {
    let all = scenarios();
    let lines = probe(&all);
    for line in &lines {
        assert!(line.contains("state=ready"), "not drawn in time: {line}");
        assert!(
            line.contains("above=true"),
            "summary not above the sections: {line}"
        );
    }
    let find = |idx: usize| {
        lines
            .iter()
            .find(|l| l.contains(&format!("idx={idx} ")))
            .unwrap()
    };
    let summary = |idx: usize| summarize(&all[idx].result, "09:05").1;

    // Clean report: the explicit all-zero line, plus the real report still drawn.
    assert_eq!(
        summary(0),
        "Last scan: 09:05, scan #5: 0 new, 0 still outstanding, 2 informational, 2 skipped"
    );
    for expected in [
        &summary(0)[..],
        "Secure Boot is currently enabled",
        "kernel lockdown is currently",
        "file_drift",
        "rootkit_signature",
        "Informational (2)",
        "Skipped / unavailable checks (2)",
        "New anomalies (0)",
    ] {
        assert!(
            find(0).contains(expected),
            "missing {expected:?} in: {}",
            find(0)
        );
    }
    // Findings.
    assert!(
        summary(1).contains("1 new, 0 still outstanding"),
        "{}",
        summary(1)
    );
    assert!(find(1).contains(&summary(1)), "{}", find(1));
    // Error reply.
    assert_eq!(
        summary(2),
        "Last scan: 09:05, did not complete: Host scanner setup is required."
    );
    assert!(find(2).contains(&summary(2)), "{}", find(2));
}
