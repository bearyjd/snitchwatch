//! The exception-first default (clarity 0c) through the real model and
//! `ConnectionsFilterBar`: nothing collapses before the first post-connect
//! snapshot's rules (the baseline), then only rows allowed by an enabled
//! recommended rule for a program seen before do; a program first seen later
//! is new until "Mark as seen"; a reconnect resets neither; disabling or
//! dropping the rule shows its rows again; "Full list", "Show all" and the
//! group's Expand/Collapse drive one model bool. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_attention_probe.qml";
const DONE_MARKER: &str = "ATTENTION_PROBE_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900
    height: 600
    property var failures: []
    property int phase: 0
    readonly property string rec: "snitchwatch-default-chronyd"

    ConnectionsModel { id: model }
    ConnectionsFilterBar {
        id: bar
        width: parent.width
        model: model
    }

    function row(id, path, action, rule, extra) {
        const r = { id: id, process: "p-" + id, processPath: path, dstHost: "h.example",
                    dstIp: "10.0.0.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                    action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: 1000,
                    matchedRule: rule };
        for (const k in (extra || {})) r[k] = extra[k];
        return r;
    }
    // One row per invariant case; only "handled" may collapse.
    function snapshotRows() {
        return [
            probe.row("handled", "/usr/sbin/chronyd", "allow", probe.rec),
            probe.row("pending", "/usr/sbin/chronyd", null, null),
            probe.row("denied", "/usr/sbin/chronyd", "deny", probe.rec),
            probe.row("user", "/usr/sbin/chronyd", "allow", "allow-ntp-host"),
            probe.row("nopath", null, "allow", probe.rec),
            probe.row("deferred", "/usr/sbin/chronyd", "allow", probe.rec, { deferred: true }),
            probe.row("default", "/usr/sbin/chronyd", "allow", probe.rec, { decidedByDefault: true }),
        ];
    }
    function send(msg) { model.applyServerMessageJson(JSON.stringify(msg)); }
    function rules(enabled) {
        probe.send({ action: "setRules", rules: [
            { name: probe.rec, enabled: enabled, action: "allow" },
            { name: "allow-ntp-host", enabled: true, action: "allow" }] });
    }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function named(item, name) {
        if (item.objectName === name) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.named(kids[i], name);
            if (f) return f;
        }
        return null;
    }
    function shows(id) { return model.visibleRowIndex(id) >= 0; }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const full = probe.named(bar, "fullListSwitch");
                if (probe.phase === 0) {
                    model.setGroupedMode(false);
                    probe.expect(model.filterNeedsAttention, "needs attention is not on by default");
                    probe.expect(full !== null && !full.checked, "no unchecked Full list switch");
                    // Rules before any snapshot: no baseline, nothing collapses.
                    probe.send({ action: "insertConnectionRows", rows: probe.snapshotRows() });
                    probe.rules(true);
                    probe.expect(model.backgroundCount === 0 && model.matchingCount === 7,
                                 "collapsed before the baseline: " + model.matchingCount);
                    probe.expect(model.baselineNotice === "", "baseline before a snapshot");
                    // The first post-connect snapshot: clear, replay, rules.
                    probe.send({ action: "clearConnectionRows" });
                    probe.send({ action: "insertConnectionRows", rows: probe.snapshotRows() });
                    probe.expect(model.backgroundCount === 0, "collapsed before the snapshot's rules");
                    probe.rules(true);
                    probe.expect(model.baselineNotice.indexOf("Baseline recorded") >= 0,
                                 "no baseline notice: " + model.baselineNotice);
                    probe.expect(probe.named(bar, "baselineNotice").visible, "notice row hidden");
                    probe.expect(model.backgroundCount === 1 && model.backgroundPrograms === 1,
                                 "background: " + model.backgroundCount + "/" + model.backgroundPrograms);
                    probe.expect(!probe.shows("handled"), "the handled row still shows");
                    for (const id of ["pending", "denied", "user", "nopath", "deferred", "default"])
                        probe.expect(probe.shows(id), id + " was collapsed");
                    probe.expect(model.matchingCount === 6 && model.totalCount === 7, "counts");
                    probe.expect(probe.named(bar, "showingText").text === "Showing 6 of 7 connections",
                                 "showing: " + probe.named(bar, "showingText").text);
                    const summary = probe.named(bar, "backgroundSummary");
                    probe.expect(summary.visible && summary.text.indexOf(
                        "Background and system: 1 connection from 1 program") === 0,
                        "summary: " + summary.text);
                    probe.expect(probe.named(bar, "backgroundToggle").text === "Expand", "toggle text");
                    probe.expect(model.filterSummary === "", "the default adds filter text");
                } else if (probe.phase === 1) {
                    // A program first seen after the baseline is new.
                    probe.send({ action: "insertConnectionRows", rows: [
                        probe.row("late", "/usr/sbin/late", "allow", probe.rec)] });
                    probe.expect(model.newProgramCount === 1, "new: " + model.newProgramCount);
                    probe.expect(probe.shows("late"), "a first-seen program collapsed");
                    probe.expect(probe.named(bar, "newProgramsRow").visible, "new programs row hidden");
                    // A reconnect: clear and replay reset neither.
                    probe.send({ action: "clearConnectionRows" });
                    const replay = probe.snapshotRows();
                    replay.push(probe.row("late", "/usr/sbin/late", "allow", probe.rec));
                    replay.push(probe.row("other", "/usr/sbin/other", "allow", probe.rec));
                    probe.send({ action: "insertConnectionRows", rows: replay });
                    probe.rules(true);
                    probe.expect(model.newProgramCount === 2 && probe.shows("late") && probe.shows("other"),
                                 "a reconnect re-baselined: " + model.newProgramCount);
                    probe.expect(model.backgroundCount === 1, "reconnect background: " + model.backgroundCount);
                    probe.named(bar, "markSeenButton").clicked();
                    probe.expect(model.newProgramCount === 0, "Mark as seen kept flags");
                    probe.expect(model.backgroundCount === 3 && !probe.shows("late"),
                                 "acknowledged programs did not collapse: " + model.backgroundCount);
                } else if (probe.phase === 2) {
                    // A disabled rule, then an empty (unknown) list: fail open.
                    probe.rules(false);
                    probe.expect(model.backgroundCount === 0 && probe.shows("handled"),
                                 "a disabled rule still collapses");
                    probe.send({ action: "setRules", rules: [] });
                    probe.expect(model.matchingCount === model.totalCount, "empty rules collapse rows");
                    probe.expect(!model.filterActive, "the empty-rules default narrows the list");
                    probe.rules(true);
                    probe.expect(!probe.shows("handled"), "re-enabled rule does not collapse");
                    // One bool behind Full list, Show all and Expand/Collapse.
                    full.click();
                    probe.expect(!model.filterNeedsAttention && probe.shows("handled"), "Full list");
                    probe.expect(probe.named(bar, "backgroundToggle").text === "Collapse", "expanded text");
                    probe.expect(!probe.named(bar, "showingRow").visible, "showing row while expanded");
                    probe.named(bar, "backgroundToggle").clicked();
                    probe.expect(model.filterNeedsAttention && !full.checked && !probe.shows("handled"),
                                 "Collapse");
                    probe.named(bar, "showAllButton").clicked();
                    probe.expect(!model.filterNeedsAttention && full.checked, "Show all");
                    probe.named(bar, "dismissBaseline").clicked();
                    probe.expect(model.baselineNotice === "" && !probe.named(bar, "baselineNotice").visible,
                                 "Dismiss");
                    done = true;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("exception: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("attention probe: " + probe.failures.join("; "));
                    }
                    console.warn("ATTENTION_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn exception_first_default_collapses_only_handled_rows() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(QML), &QUrl::from(PROBE_URL));
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
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "attention probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
