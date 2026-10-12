//! `ConnectionsModel`'s process-identity roles reach QML: `processPath`,
//! `direction`, `startedAtMs` and `appIcon`, in the flat view and on the
//! grouped view's leaf rows (headers report empty / -1). A Repeater delegate
//! reads each role; failures are thrown against the probe's URL, which the
//! stderr capture turns into an assertion. Also covers demo mode being inert
//! without `SNITCHWATCH_DEMO=1`. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_process_roles_probe.qml";
const DONE_MARKER: &str = "PROCESS_ROLES_PROBE_DONE";

#[test]
fn process_identity_roles_are_readable_from_qml() {
    init_headless_qt_env();
    // Demo mode must be off for the "inert" assertion below.
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
    width: 300
    height: 300
    property var seen: []
    property var failures: []
    property int phase: 0

    ConnectionsModel { id: model }
    Repeater {
        model: model
        delegate: Item {
            required property string rowId
            required property bool isGroupHeader
            required property string processPath
            required property string direction
            required property real startedAtMs
            required property string appIcon
            Component.onCompleted: probe.seen.push({
                id: rowId, header: isGroupHeader, path: processPath,
                direction: direction, started: startedAtMs, icon: appIcon })
        }
    }

    function row(id, path, direction, started) {
        return { id: id, process: "p", processPath: path, dstHost: "h.example",
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: direction,
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: started };
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
                    model.setGroupedMode(false);
                    model.loadDemoRows();
                    probe.expect(model.totalCount === 0, "loadDemoRows must be a no-op without SNITCHWATCH_DEMO=1");
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [
                            probe.row("a", "/usr/sbin/unknown-daemon", "incoming", 1760000000123),
                            probe.row("b", null, "outgoing", 5)
                        ]
                    }));
                } else {
                    const a = probe.seen.find(s => s.id === "a");
                    const b = probe.seen.find(s => s.id === "b");
                    probe.expect(!!a && !!b, "both rows reach the delegate");
                    if (a && b) {
                        probe.expect(a.path === "/usr/sbin/unknown-daemon", "processPath: " + a.path);
                        probe.expect(a.direction === "incoming", "direction: " + a.direction);
                        probe.expect(a.started === 1760000000123, "startedAtMs: " + a.started);
                        probe.expect(a.icon === "application-x-executable", "unmatched path gets the generic icon: " + a.icon);
                        probe.expect(b.path === "", "missing path is empty: '" + b.path + "'");
                        probe.expect(b.icon === "application-x-executable", "no path gets the generic icon: " + b.icon);
                    }
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
                        throw new Error("process roles probe: " + probe.failures.join("; "));
                    }
                    console.warn("PROCESS_ROLES_PROBE_DONE");
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
        "process roles probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
