//! Integration smoke: `TrafficModel` is usable as a QML element (Task 11).
//!
//! Scope of this test — deliberately, only this (mirrors
//! `blocklists_model_qml.rs`'s scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> ring-store-apply -> series/label property path without
//!     aborting (a Rust panic inside the invokable would abort this test
//!     binary), and
//!   * `TrafficPage.qml` itself compiles and instantiates — issue #19
//!     rebuilt the page around daemon aggregate stats (a stat-tile grid)
//!     instead of the old `Canvas`-based byte-rate chart. Unlike an earlier
//!     version of this test, `TrafficPage` is the *root* document below (the
//!     same convention `rules_page_diagnostics_qml.rs`/
//!     `connections_page_diagnostics_qml.rs` use), not created via a nested
//!     `Qt.createComponent()` whose result was never asserted on — a
//!     type/property error or failed import in `TrafficPage.qml` now nulls
//!     the root object directly, which the `root_ok` assertion below does
//!     check.
//!
//! It intentionally does NOT assert series/label content: a thrown JS error
//! in `Component.onCompleted` does not null the root object, so QML-side
//! asserts are not load-bearing. The ring store's fold/formatting
//! *correctness* is covered exhaustively and Qt-free by
//! `traffic::ring_store`'s unit tests, which are what Task 11's acceptance
//! criterion calls for. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::traffic_model as _;

#[test]
fn traffic_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // `TrafficPage` is the root document (not a nested Qt.createComponent()
    // result whose status was never checked) — see the module doc comment.
    // Drives a batch of TrafficEvents plus one DaemonStatistics message
    // through the real model so both the (retained) series plumbing and the
    // new stat-tile grid exercise for real. If an invokable panicked, the
    // whole test binary would abort; if the type were unregistered or the
    // page failed to compile, `root_ok` stays false.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

TrafficPage {
    id: page
    model: TrafficModel {
        id: trafficModel

        Component.onCompleted: {
            const events = [];
            for (let i = 0; i < 90; i++) {
                events.push({
                    timestampMs: 1000000000000 + i * 1000,
                    bytesIn: 100 + i,
                    bytesOut: 50 + i
                });
            }
            trafficModel.applyServerMessageJson(JSON.stringify({
                action: "trafficEvents",
                events: events
            }));

            // Issue #19: the page now renders around daemon aggregate stats
            // rather than the byte-rate series above — feed one so the
            // page's stat-tile grid (not just its placeholder) exercises
            // for real.
            trafficModel.applyServerMessageJson(JSON.stringify({
                action: "daemonStatistics",
                daemonVersion: "1.8.0",
                uptime: 3661,
                rules: 12,
                connections: 4200,
                ignored: 10,
                accepted: 4000,
                dropped: 200,
                ruleHits: 3900,
                ruleMisses: 300
            }));

            // Best-effort visibility only (not load-bearing — see module docs).
            console.log("[test] TrafficModel.count after insert =", trafficModel.count,
                        "currentInLabel =", trafficModel.currentInLabel,
                        "currentOutLabel =", trafficModel.currentOutLabel,
                        "statsReceived =", trafficModel.statsReceived,
                        "daemonVersion =", trafficModel.daemonVersion);
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

    if let Some(engine) = engine.as_mut() {
        engine.load_data(
            &QByteArray::from(qml),
            &QUrl::from("qrc:/inline_traffic_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "TrafficPage QML probe failed: root object was null — the TrafficModel type failed to \
         register as a QML element, or TrafficPage.qml did not compile"
    );

    let _ = app.as_mut();
}
