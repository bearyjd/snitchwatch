//! The grouped view's expand/collapse and filter changes reach QML as ranged
//! model signals (rowsInserted / rowsRemoved / dataChanged), never a model
//! reset, so a view keeps its scroll position and selection. A probe window
//! counts each signal around the toggles; failures are thrown against the
//! probe's URL, which the stderr capture turns into an assertion. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_ranged_toggle_probe.qml";
const DONE_MARKER: &str = "RANGED_TOGGLE_PROBE_DONE";

#[test]
fn grouped_toggles_and_filters_use_ranged_signals_not_resets() {
    init_headless_qt_env();
    // Demo mode must be off for the "inert" assertion below.
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 800
    height: 600
    property var failures: []
    property int phase: 0
    property int resets: 0
    property var inserted: []
    property var removed: []
    property int changed: 0
    // This probe clicks a row 5 deep in the list; the featured map band would
    // push it below the 600 px window. Collapse state is session-only.
    Component.onCompleted: MapBandState.collapsed = true

    ConnectionsModel { id: model }
    QtObject {
        id: feedStub
        property bool ok: true
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {}
    }
    // The real page and ConnectionRow delegate over the same model, to prove a
    // selection survives a collapse above it.
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: model
    }
    TestCase { id: tc; name: "ranged"; when: false }
    function findList(item) {
        if (typeof item.itemAtIndex === "function" && item.currentIndex !== undefined) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findList(kids[i]);
            if (f) return f;
        }
        return null;
    }
    Connections {
        target: model
        function onModelReset() { probe.resets++; }
        function onRowsInserted(parent, first, last) { probe.inserted.push([first, last]); }
        function onRowsRemoved(parent, first, last) { probe.removed.push([first, last]); }
        function onDataChanged() { probe.changed++; }
    }

    function row(id, host) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function mark() { probe.inserted = []; probe.removed = []; probe.changed = 0; probe.resets = 0; }
    function span(list) { return JSON.stringify(list); }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com"),
                               probe.row("c", "y.example.com")]
                    }));
                } else if (probe.phase === 1) {
                    // Lands the debounced insert (its own reset is allowed).
                    model.setPendingOnly(false);
                    probe.expect(model.count === 1, "one collapsed program header, got " + model.count);
                    probe.mark();
                    model.toggleProcessGroup("p");
                    probe.expect(probe.resets === 0, "expanding a program reset the model");
                    probe.expect(probe.span(probe.inserted) === "[[1,1]]",
                                 "program expand inserts the domain header: " + probe.span(probe.inserted));
                    probe.expect(probe.changed === 1, "program header repainted once: " + probe.changed);
                    probe.expect(model.count === 2, "count after expand: " + model.count);
                    probe.mark();
                    model.toggleDomainGroup("p", "example.com");
                    probe.expect(probe.resets === 0, "expanding a domain reset the model");
                    probe.expect(probe.span(probe.inserted) === "[[2,3]]",
                                 "domain expand inserts its two host headers: " + probe.span(probe.inserted));
                    probe.expect(model.count === 4, "count after domain expand: " + model.count);
                    probe.mark();
                    model.toggleHostGroup("p", "example.com", "y.example.com");
                    probe.expect(probe.resets === 0, "expanding a host reset the model");
                    probe.expect(probe.span(probe.inserted) === "[[4,5]]",
                                 "host expand inserts its two connections: " + probe.span(probe.inserted));
                    probe.expect(probe.removed.length === 0, "host expand removed rows");
                    probe.expect(probe.changed === 1, "host header repainted once: " + probe.changed);
                    probe.expect(model.count === 6, "count after host expand: " + model.count);
                    probe.mark();
                    model.toggleHostGroup("p", "example.com", "y.example.com");
                    probe.expect(probe.resets === 0, "collapsing a host reset the model");
                    probe.expect(probe.span(probe.removed) === "[[4,5]]",
                                 "host collapse removes its two connections: " + probe.span(probe.removed));
                    probe.expect(probe.inserted.length === 0, "host collapse inserted rows");
                    probe.expect(model.count === 4, "count after host collapse: " + model.count);
                    probe.mark();
                    model.toggleDomainGroup("p", "example.com");
                    probe.expect(probe.resets === 0, "collapsing a domain reset the model");
                    probe.expect(probe.span(probe.removed) === "[[2,3]]",
                                 "domain collapse removes its two hosts: " + probe.span(probe.removed));
                    probe.expect(probe.inserted.length === 0, "collapse inserted rows");
                    probe.expect(model.count === 2, "count after collapse: " + model.count);
                    probe.mark();
                    model.setFilterQuery("x.example");
                    probe.expect(probe.resets === 0, "a grouped filter reset the model");
                    probe.expect(model.count === 4, "filter keeps program, domain, host and the match: " + model.count);
                    probe.expect(probe.inserted.length === 1, "filter opens one window: " + probe.span(probe.inserted));
                    probe.mark();
                    model.setFilterQuery("");
                    probe.expect(probe.resets === 0, "clearing the filter reset the model");
                    probe.expect(model.count === 2, "count after clearing: " + model.count);
                    probe.mark();
                    model.toggleDomainGroup("p", "example.com");
                    model.toggleHostGroup("p", "example.com", "y.example.com");
                    probe.expect(model.count === 6, "count with a host open: " + model.count);
                    probe.mark();
                    model.toggleDomainGroup("p", "example.com");
                    probe.expect(probe.resets === 0, "collapsing a domain with an open host reset the model");
                    probe.expect(probe.span(probe.removed) === "[[2,5]]",
                                 "domain collapse removes its hosts and the open host's rows: "
                                 + probe.span(probe.removed));
                    probe.expect(probe.inserted.length === 0, "open-host collapse inserted rows");
                    probe.expect(model.count === 2, "count after collapsing an open-host domain: " + model.count);
                } else if (probe.phase === 2) {
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("sel", "s.example.com")].map(r => Object.assign(r, { process: "q" }))
                    }));
                } else if (probe.phase === 3) {
                    model.setPendingOnly(false);
                    model.toggleProcessGroup("q");
                    model.toggleDomainGroup("q", "example.com");
                    model.toggleHostGroup("q", "example.com", "s.example.com");
                    // p, p's collapsed domain, q, q's domain, q's host, sel.
                    probe.expect(model.count === 6, "count before selecting: " + model.count);
                } else if (probe.phase === 4) {
                    const list = probe.findList(page);
                    probe.expect(list !== null, "no ListView under the page");
                    const item = list ? list.itemAtIndex(5) : null;
                    probe.expect(item !== null && item.rowId === "sel", "row sel is not at index 5");
                    if (item) {
                        tc.mouseClick(item, 2, 2);
                        probe.expect(list.currentIndex === 5, "click did not select: " + list.currentIndex);
                    }
                    model.toggleProcessGroup("p");
                    probe.expect(model.count === 5, "count after collapsing p: " + model.count);
                } else if (probe.phase === 5) {
                    const list = probe.findList(page);
                    const cur = list.itemAtIndex(list.currentIndex);
                    // Never a different connection: either still "sel" or cleared.
                    probe.expect(list.currentIndex === -1 || (cur && cur.rowId === "sel"),
                                 "selection moved to another row: index " + list.currentIndex
                                 + " id " + (cur ? cur.rowId : "none"));
                    done = true;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("ranged toggle probe: " + probe.failures.join("; "));
                    }
                    console.warn("RANGED_TOGGLE_PROBE_DONE");
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
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "ranged toggle probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
