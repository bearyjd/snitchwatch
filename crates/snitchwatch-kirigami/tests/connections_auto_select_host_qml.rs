//! A new pending row under an open domain opens its host and is auto-selected.
//! The ranged signals for the host-open are guarded by the debug assert in
//! `apply_grouped_projection`, not observed here (the insert itself resets the
//! model in this harness). Shares the ranged-toggle probe harness: failures are
//! thrown against the probe's URL, which the stderr capture turns into an
//! assertion. Run headless with `QT_QPA_PLATFORM=offscreen`.
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
fn a_new_pending_row_under_an_open_domain_opens_its_host_and_is_auto_selected() {
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
    width: 800
    height: 600
    property var failures: []
    property int phase: 0
    property int resets: 0
    property var autos: []

    ConnectionsModel { id: model }
    Connections {
        target: model
        function onModelReset() { probe.resets++; }
        function onAutoSelectRequested(row) { probe.autos.push(row); }
    }

    function row(id, host, action) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: action, bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function insert(r) {
        model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [r] }));
    }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    probe.insert(probe.row("a", "x.example.com", "allow"));
                } else if (probe.phase === 1) {
                    model.setPendingOnly(false);
                    model.toggleProcessGroup("p");
                    probe.expect(model.count === 2, "program open, domain collapsed: " + model.count);
                    probe.autos = [];
                } else if (probe.phase === 2) {
                    probe.insert(probe.row("m", "w.example.com", null));
                } else if (probe.phase === 3) {
                    model.setPendingOnly(false);
                    // The user collapsed the domain: nothing is forced open.
                    probe.expect(model.count === 2, "collapsed domain stayed collapsed: " + model.count);
                    probe.expect(probe.autos.length === 0, "selected under a collapsed domain: " + probe.autos);
                    model.toggleDomainGroup("p", "example.com");
                    probe.expect(model.count === 4, "domain open shows two hosts: " + model.count);
                    probe.autos = [];
                    probe.resets = 0;
                } else if (probe.phase === 4) {
                    probe.insert(probe.row("n", "z.example.com", null));
                } else if (probe.phase === 5) {
                    model.setPendingOnly(false);
                    // p, domain, hosts x, w, z, and n under the opened z.
                    probe.expect(model.count === 6, "host z opened for the new pending row: " + model.count);
                    probe.expect(probe.autos.length === 1 && probe.autos[0] === 5,
                                 "auto-select of the new row: " + JSON.stringify(probe.autos));
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
                        throw new Error("auto-select probe: " + probe.failures.join("; "));
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
