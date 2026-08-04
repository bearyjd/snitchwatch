//! Integration smoke: issue #18's inline Allow/Deny buttons + per-process
//! batch actions on `ConnectionsPage.qml`.
//!
//! Scope of this test — same convention as `connections_page_diagnostics_qml.rs`:
//! it instantiates the *real* `ConnectionsPage.qml`, feeds it a pending row via
//! the real `ConnectionsModel`, and drives the exact functions the inline
//! Allow/Deny buttons and the process-header "Allow all"/"Deny all" buttons
//! call (`submitInlineVerdict` / `submitBatchVerdict`) — the same click-path
//! entry points wired into the delegate's `onClicked` handlers. This is a
//! click-path exercise (handler -> `bridgeFeed.submitVerdict` -> recorded
//! tokens), not a synthesized mouse event.
//!
//! **The probe must supply a non-null `bridgeFeed`.** `submitInlineVerdict`
//! is wrapped in a `page.bridgeFeed !== null` guard, and `submitBatchVerdict`
//! funnels through it, so a null feed skips the entire verdict path and
//! leaves this test asserting nothing beyond "the page parsed." The stub
//! below records what it receives, and the `Timer` asserts both the inline
//! and batch paths actually arrived carrying the sheet's default
//! scope/duration tokens — the contract `ConnectionsPage.submitInlineVerdict`
//! documents and `pending_decision.rs` pins on the Rust side.
//!
//! Honoring the "QML-side JS asserts are not load-bearing" constraint, this
//! test's Rust-side assertion works in two layers:
//!
//!   1. The probe root is a real `Window { visible: true; ... }` (not a
//!      bare, unparented `ConnectionsPage`), and the whole scene is driven
//!      through a real Qt event loop via `QGuiApplication::exec()` (a QML
//!      `Timer` quits it once the click paths have run) — the offscreen QPA
//!      platform supports this without a real display. That matters because
//!      `QQuickListView` only instantiates its delegates (and evaluates
//!      their bindings, e.g. the new "Allow all (" + row.groupPending + ")"
//!      label expression) during layout/polish passes that happen on the
//!      event loop, not synchronously during `load_data`. Without pumping
//!      the loop, a broken delegate-local binding would never actually run
//!      and this test would falsely pass.
//!   2. Around the `load_data`/`exec()` window, fd 2 (stderr) is redirected
//!      via `libc::dup`/`dup2` into a tempfile (Qt's default message handler
//!      writes `qWarning`/`qCritical` — including QML JS exceptions — to
//!      stderr via the C `FILE*` stream, which respects fd redirection).
//!      After restoring stderr, the captured text is asserted to contain no
//!      line naming this probe's own QML URL: a QML JS error (e.g.
//!      `TypeError: Property 'submitInlineVerdict' ... is not a function`,
//!      or a broken binding in the new buttons) is reported by Qt with that
//!      URL as a prefix, so this positively fails on a broken click path
//!      rather than only checking "did the process not crash." A genuinely
//!      null root object (a QML *parse* error, e.g. a syntax error or an
//!      unregistered/misspelled type — as opposed to a JS runtime error
//!      inside a handler, which does NOT null the root) is asserted
//!      separately below, mirroring `connections_page_diagnostics_qml.rs`.
//!
//! The produced `SetVerdict` JSON's *content* — including that it carries
//! the sheet's own default scope/duration tokens (`this_host`/`this_time`)
//! — is asserted exhaustively and Qt-free by `pending_decision.rs`'s
//! `build_message_serializes_to_expected_json` test (which encodes the same
//! tokens `submitInlineVerdict` passes) and by
//! `connections::grouping::tests::pending_row_ids_compose_into_valid_batch_deny_messages`
//! (which pins the batch-action token pair). Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/inline_verdict_probe.qml";

#[test]
fn inline_and_batch_verdict_click_paths_run_without_erroring() {
    init_headless_qt_env();
    // Deliberately NOT setting QT_FATAL_WARNINGS: a pre-existing upstream
    // Kirigami OverlaySheet binding-loop warning would abort the whole test
    // binary under it, unrelated to anything this test is checking.

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 800
    height: 600

    // Recording stand-in for BridgeFeed. `ConnectionsPage.bridgeFeed` is a
    // plain `var`, so any QObject exposing `submitVerdict` satisfies the
    // page's null guard — and satisfying it is the whole point: with a null
    // feed, submitInlineVerdict returns early and nothing downstream runs.
    QtObject {
        id: feedStub
        property int allowCount: 0
        property int denyCount: 0
        property string lastScope: ""
        property string lastDuration: ""

        function submitVerdict(rowId, choice, scope, duration) {
            if (choice === "allow") {
                feedStub.allowCount++;
            } else if (choice === "deny") {
                feedStub.denyCount++;
            }
            feedStub.lastScope = scope;
            feedStub.lastDuration = duration;
        }
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel {
            id: connModel
            Component.onCompleted: {
                setGroupedMode(true);
                applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        { id: "r1", process: "curl", processPath: null, dstHost: "github.com",
                          dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                          action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                          matchedRule: null }
                    ]
                }));
            }
        }
        Component.onCompleted: {
            // Exercise the inline-button click path directly (the delegate's
            // Allow/Deny buttons call exactly this function with the row id).
            page.submitInlineVerdict("r1", "allow");

            // Exercise the process-header batch-action click path: re-seed a
            // fresh pending row, then batch-decide the whole "curl" process
            // group the same way "Allow all" would.
            connModel.applyServerMessageJson(JSON.stringify({
                action: "insertConnectionRows",
                rows: [
                    { id: "r2", process: "curl", processPath: null, dstHost: "example.com",
                      dstIp: "2.2.2.2", dstPort: 443, protocol: "tcp", direction: "outgoing",
                      action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                      matchedRule: null }
                ]
            }));
            page.submitBatchVerdict("curl", "deny");
        }
    }

    // Quits the event loop once the delegate layout/polish pass (and any JS
    // errors it would surface) has had a chance to run — see the module doc
    // comment above for why pumping the loop matters here.
    //
    // Also where the verdict path is actually asserted. A `throw` here is
    // reported by Qt as a JS error prefixed with this probe's URL, which the
    // Rust-side stderr assertion below treats as a failure — so the check is
    // load-bearing in Rust, not a QML-side assert. `finally` guarantees
    // Qt.quit() still runs on the failing path; without it a throw would
    // leave the event loop spinning and hang the test binary.
    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                if (feedStub.allowCount < 1) {
                    throw new Error("inline Allow never reached bridgeFeed.submitVerdict");
                }
                if (feedStub.denyCount < 1) {
                    throw new Error("batch Deny never reached bridgeFeed.submitVerdict");
                }
                if (feedStub.lastScope !== "this_host" || feedStub.lastDuration !== "this_time") {
                    throw new Error("verdict carried unexpected default tokens: "
                                    + feedStub.lastScope + "/" + feedStub.lastDuration);
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
        "ConnectionsPage QML probe failed: root object was null — a QML *parse* error (syntax \
         error, unregistered/misspelled type) in the probe or ConnectionsPage.qml. (A JS runtime \
         error inside a handler/binding does NOT null the root — that's what the stderr capture \
         below catches instead.)"
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) reported against the probe URL while exercising the inline/batch \
         verdict click paths — this covers both a broken binding/handler AND the probe's own \
         Timer assertions that the inline Allow and batch Deny actually reached \
         bridgeFeed.submitVerdict carrying the this_host/this_time defaults. Captured \
         stderr:\n{}",
        bad_lines.join("\n")
    );
}
