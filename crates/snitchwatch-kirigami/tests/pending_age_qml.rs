//! Integration smoke: `ConnectionsModel.oldestPendingAgeSecs` /
//! `refreshPendingAge()` — the pending-decision-exposure warning's Rust->QML
//! contract (see
//! docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md).
//!
//! `qml_source_guards.rs`'s `pending_exposure_banner_stays_wired_to_oldest_pending_age`
//! pins the QML *half* of this contract (the banner's `visible` binding and
//! the poll `Timer`'s call) by text match against `main.qml`. It cannot catch
//! a Rust-side rename of the `cxx_name`s themselves (`oldestPendingAgeSecs`,
//! `refreshPendingAge`) — a QML binding onto a nonexistent property silently
//! evaluates to `undefined`, and `tests/smoke.rs` never spins an event loop
//! long enough to notice a misnamed invokable. This test closes that gap by
//! actually calling `refreshPendingAge()` and asserting the real property
//! value through a real Qt event loop, mirroring `inline_verdict_qml.rs`'s
//! stderr-capture pattern for a load-bearing (not merely QML-side) assertion.
//!
//! `pending_age_secs`'s own arithmetic (elapsed-seconds computation, the
//! sentinel, and the clock-skew saturation) is covered exhaustively and
//! Qt-free by `connections_model.rs`'s own unit tests; this test only proves
//! the property/invokable wiring works end-to-end under those exact names.
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::connections_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/pending_age_probe.qml";

#[test]
fn oldest_pending_age_secs_reflects_a_real_pending_row_and_resets_on_resolution() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    // `startedAtMs` is 15s in the past (relative to Date.now() at load time)
    // so a single refreshPendingAge() call deterministically reports an age
    // >= 10 without racing a real wall-clock wait in the test.
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeRoot
    visible: true
    width: 1
    height: 1
    property ConnectionsModel model: ConnectionsModel {}

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                if (probeRoot.model.oldestPendingAgeSecs !== -1) {
                    throw new Error("expected -1 sentinel with nothing pending, got "
                                    + probeRoot.model.oldestPendingAgeSecs);
                }

                const pastMs = Date.now() - 15000;
                probeRoot.model.applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        { id: "r1", process: "curl", processPath: null, dstHost: "example.com",
                          dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                          action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: pastMs,
                          matchedRule: null }
                    ]
                }));
                probeRoot.model.refreshPendingAge();
                if (probeRoot.model.oldestPendingAgeSecs < 10) {
                    throw new Error("expected oldestPendingAgeSecs >= 10 after inserting a "
                                    + "15s-old pending row, got "
                                    + probeRoot.model.oldestPendingAgeSecs);
                }

                probeRoot.model.applyServerMessageJson(JSON.stringify({
                    action: "updateConnectionRows",
                    rows: [
                        { id: "r1", process: "curl", processPath: null, dstHost: "example.com",
                          dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                          action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: pastMs,
                          matchedRule: null }
                    ]
                }));
                if (probeRoot.model.oldestPendingAgeSecs !== -1) {
                    throw new Error("expected -1 sentinel after the only pending row resolved, "
                                    + "got " + probeRoot.model.oldestPendingAgeSecs);
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
        "ConnectionsModel pending-age QML probe failed: root object was null — a QML parse \
         error (syntax error, or oldestPendingAgeSecs/refreshPendingAge unregistered/misspelled \
         on the Rust side)."
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) reported against the probe URL — oldestPendingAgeSecs/\
         refreshPendingAge did not behave as the pending-decision-exposure banner requires. \
         Captured stderr:\n{}",
        bad_lines.join("\n")
    );
}
