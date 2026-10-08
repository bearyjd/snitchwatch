//! Integration smoke: `BridgeFeed` is usable as a QML element and its
//! `submitVerdict` invokable runs the real token-parsing path.
//!
//! Replaces the coverage the deleted `pending_decision_qml.rs` used to give.
//! When verdicts routed through the `PendingDecision` QObject, that probe
//! proved the wrapper registered under `com.snitchwatch.shell` and that its
//! `submit` invokable ran `build_verdict_message` without aborting. Verdicts
//! now go through `BridgeFeed::submitVerdict` instead, so the same guarantee
//! has to be re-established here — otherwise a renamed `#[cxx_name]`, a
//! changed arity, or a panic inside the invokable reaches a user's Allow/Deny
//! click with nothing in the suite to catch it.
//!
//! Scope of this test — deliberately, only this:
//!   * the `BridgeFeed` cxx-qt wrapper registers as a QML type and
//!     instantiates (a null root means it failed to register or compile), and
//!   * `submitVerdict` is callable from QML with its documented four-token
//!     signature plus the row's `bindableProcessPath` flag (a different arity
//!     throws) and drives the real
//!     `pending_decision::build_verdict_message` path without panicking (a
//!     Rust panic inside the invokable aborts this test binary), and
//!   * an unrecognised choice token is *rejected in Rust* rather than
//!     dispatched — the conservative branch `pending_decision.rs` guarantees.
//!
//! No bridge runtime is started, so `dispatch()` takes its documented
//! `bridge_runtime::handles() == None` path and logs-and-drops. That is the
//! point: this probe exercises the QML->Rust boundary, while what happens on
//! the far side of the channel is covered Qt-free by `pending_decision.rs`'s
//! unit tests and by the bridge's own round-trip tests.
//!
//! **Two harness constraints this probe is shaped around**, both verified by
//! deliberately breaking this test and confirming it goes red:
//!
//!   1. The root must be a `Window` driven through `QGuiApplication::exec()`.
//!      With a bare `QtObject` root, `QQmlApplicationEngine::load_data`
//!      reports a non-null root but **never runs `Component.onCompleted`**,
//!      so every call below would silently not happen and this file would
//!      assert nothing.
//!   2. A QML JS error (wrong invokable name/arity) does NOT null the root
//!      object, so the stderr capture is what catches it — and that capture
//!      only works because [`common::init_headless_qt_env`] sets
//!      `QT_FORCE_STDERR_LOGGING`. See its doc comment.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/bridge_feed_probe.qml";

#[test]
fn bridge_feed_submit_verdict_invokable_is_callable_from_qml() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    // Every scope/duration token pair the sheet can produce, plus the
    // inline/batch defaults, plus a deliberately bogus choice token. All of
    // them cross the cxx-qt boundary for real; the bogus one must be dropped
    // Rust-side by `VerdictChoice::from_token` rather than throwing here.
    //
    // `finally` guarantees Qt.quit() runs even if a call throws — without it
    // a failure hangs the event loop instead of failing the test.
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 400
    height: 300

    property BridgeFeed feed: BridgeFeed {}

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                // The inline/batch default pair used by ConnectionsPage.
                probeWindow.feed.submitVerdict("r1", "allow", "this_host", "this_time", true);
                probeWindow.feed.submitVerdict("r2", "deny", "this_host", "this_time", true);

                // Every remaining scope/duration token the sheet offers.
                probeWindow.feed.submitVerdict("r3", "allow", "any_host_on_domain", "for_5_minutes", true);
                probeWindow.feed.submitVerdict("r4", "deny", "any_host", "until_quit", true);
                probeWindow.feed.submitVerdict("r5", "allow", "any_host", "forever", false);

                // Unrecognised choice: must be rejected in Rust (logged and
                // dropped), never dispatched as some other verdict.
                probeWindow.feed.submitVerdict("r6", "maybe", "this_host", "this_time", true);

                // The JSON sink the models' request signals use still works.
                probeWindow.feed.sendClientJson(JSON.stringify({
                    action: "setVerdict",
                    rowId: "r7",
                    verdict: "deny",
                    scope: "this_host",
                    duration: "once"
                }));

                // Malformed JSON there must be logged and dropped, not thrown.
                probeWindow.feed.sendClientJson("{not valid json");

                // Issue #44's verdict-not-remembered relay. main.qml calls it
                // before every model's feed, so a throw here would stop them
                // all; with no bridge runtime it logs and returns.
                probeWindow.feed.startBridgeFeed();

                // ConnectionsPage listens with `ignoreUnknownSignals` (its
                // component tests use a stub feed), so a renamed signal would
                // drop the notice silently. The real type must expose it.
                if (typeof probeWindow.feed.verdictNotRemembered !== "function") {
                    throw new Error("BridgeFeed has no verdictNotRemembered signal");
                }
                probeWindow.feed.verdictNotRemembered.connect(function (rowId) {});

                // Inline-Deny plan (version skew). ConnectionsPage treats a
                // missing check as "old bridge", so a rename would quietly
                // make every inline Deny once-only; with no bridge runtime it
                // must answer false, never fail open.
                if (typeof probeWindow.feed.appBoundRulesFor !== "function") {
                    throw new Error("BridgeFeed has no appBoundRulesFor");
                }
                if (probeWindow.feed.appBoundRulesFor("1:7") !== false) {
                    throw new Error("appBoundRulesFor is not false without a bridge");
                }
                // submitVerdict says whether the verdict was queued, so the
                // page can tell a lost connection from an old bridge. Nothing
                // can be queued headless.
                const queued = probeWindow.feed.submitVerdict("1:8", "deny", "this_host", "this_time", true);
                if (queued !== false) {
                    throw new Error("submitVerdict reported " + queued + " without a bridge");
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
        "BridgeFeed QML probe failed: root object was null — the BridgeFeed type failed to \
         register as a QML element under com.snitchwatch.shell, or the probe did not parse."
    );

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML runtime error(s) reported against the probe URL while calling \
         BridgeFeed.submitVerdict / sendClientJson — most likely a renamed or re-signatured \
         invokable (a JS TypeError here does not null the root object, which is why this \
         assertion exists). Captured stderr:\n{}",
        bad_lines.join("\n")
    );

    let _ = app.as_mut();
}
