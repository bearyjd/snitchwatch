//! The standalone alert (`AlertWindow.qml`, LS redesign phase 1) in a real
//! window with a real `ConnectionsModel` and a recording stand-in for
//! `BridgeFeed`:
//!   * it shows the OLDEST pending row, identity first, and moves on to the
//!     next when that row is decided;
//!   * every key and click is ignored until `armDelayMs` has passed, again
//!     after it moves on to the next row and when the offered durations
//!     appear or vanish (`handleKey` is what the window's `Keys.onPressed`
//!     calls; a source guard pins that call); an auto-repeat, and a key held
//!     since before arming, never answer;
//!   * the destination line words an incoming connection as one coming in;
//!   * Return answers Allow and Esc Deny, through `submitVerdict` with the
//!     scope, the duration (preselected from `Durations.defaultToken`) and the
//!     row's `bindableProcessPath`;
//!   * where a rule cannot be bound only "Once" is offered and sent;
//!   * a verdict that could not be queued is explained and the alert stays.
//!
//! Failures are thrown against the probe's URL, which the stderr capture turns
//! into an assertion. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/alert_window_probe.qml";
const DONE_MARKER: &str = "ALERT_WINDOW_PROBE_DONE";
const ALERT_QML: &str = include_str!("../qml/AlertWindow.qml");
const ALERT_HOST_QML: &str = include_str!("../qml/AlertHost.qml");
const MAIN_QML: &str = include_str!("../qml/main.qml");

/// The desktop-notice fallback for an unseen alert needs the alert's focus
/// state to reach `NotificationController.alertActive`.
#[test]
fn the_alerts_focus_reaches_the_notification_controller() {
    assert!(
        ALERT_HOST_QML.contains("alertWindow.visible")
            && ALERT_HOST_QML.contains("alertWindow.activeFocusItem !== null"),
        "AlertHost.alertFocused must be the alert shown with focus"
    );
    assert!(
        MAIN_QML.contains("alertActive: alertHost.alertFocused"),
        "main.qml must bind NotificationController.alertActive to the alert's focus"
    );
}

#[test]
fn the_alert_shows_the_oldest_row_ignores_input_until_armed_and_answers_through_the_feed() {
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
    width: 200
    height: 200
    property var failures: []
    property var explained: []
    property int phase: 0
    property real phaseStart: Date.now()

    QtObject {
        id: feedStub
        property var submitted: []
        property bool appBound: true
        property bool queue: true
        function submitVerdict(rowId, choice, scope, duration, bindable) {
            feedStub.submitted.push(rowId + " " + choice + " " + scope + "/" + duration
                                    + " bindable=" + bindable);
            return feedStub.queue;
        }
        function appBoundRulesFor(rowId) { return feedStub.appBound; }
    }

    ConnectionsModel { id: connModel }
    AlertWindow {
        id: alert
        model: connModel
        bridgeFeed: feedStub
        armDelayMs: 300
        onExplained: text => probe.explained.push(text)
    }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, path, started) {
        return { id: id, process: "p-" + id, processPath: path, dstHost: "example.com",
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: started,
                 answerDeadlineMs: Date.now() + 30000 };
    }
    function insert(rows) {
        connModel.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: rows }));
    }
    function decide(id, action) {
        const r = probe.row(id, "/usr/bin/x", 0);
        r.action = action;
        connModel.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [r] }));
    }
    // What `fn` sent to the feed, then cleared.
    function sent(fn) {
        feedStub.submitted = [];
        fn();
        return JSON.stringify(feedStub.submitted);
    }
    // A probe that never reaches its last phase fails instead of hanging.
    Timer {
        interval: 20000
        running: true
        onTriggered: {
            try {
                throw new Error("alert probe timed out in phase " + probe.phase
                                + " failures " + probe.failures.join("; "));
            } finally {
                Qt.quit();
            }
        }
    }
    function elapsed() { return Date.now() - probe.phaseStart; }
    function next() { probe.phase++; probe.phaseStart = Date.now(); }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    probe.expect(alert.armDelayMs === 300, "probe sets a short delay");
                    probe.expect(alert.shown === false, "nothing pending: no alert");
                    // The newer row is inserted first: age, not arrival order, decides.
                    probe.insert([probe.row("new", "/usr/bin/curl", 2000),
                                  probe.row("old", "/usr/bin/wget", 1000)]);
                    alert.refresh();
                    probe.expect(alert.rowId === "old", "oldest row first: " + alert.rowId);
                    probe.expect(alert.shown === true, "alert shown for a pending row");
                    probe.expect(alert.armed === false, "not armed on appearance");
                    probe.expect(alert.row.process === "p-old"
                                 && alert.row.processPath === "/usr/bin/wget"
                                 && alert.row.host === "example.com" && alert.row.port === 443,
                                 "identity and destination come from the row");
                    probe.expect(alert.remembers && alert.durationToken === "forever"
                                 && alert.scopeToken === "this_host",
                                 "preselected: " + alert.scopeToken + "/" + alert.durationToken);
                    probe.expect(alert.remainingSeconds > 25 && alert.remainingSeconds <= 30,
                                 "countdown from the bridge deadline: " + alert.remainingSeconds);
                    // Outgoing and incoming are worded differently.
                    probe.expect(alert.destinationText === "wants to connect to example.com:443 (tcp)",
                                 "outgoing wording: " + alert.destinationText);
                    const outgoing = alert.row;
                    alert.row = Object.assign({}, outgoing, { direction: "incoming", protocol: "udp" });
                    probe.expect(alert.destinationText === "Incoming connection from example.com (udp)",
                                 "incoming wording: " + alert.destinationText);
                    alert.row = Object.assign({}, outgoing, { direction: "incoming", host: "" });
                    probe.expect(alert.destinationText === "Incoming connection from 93.184.216.34 (tcp)",
                                 "incoming wording by address: " + alert.destinationText);
                    alert.row = outgoing;
                    // Everything is ignored while unarmed.
                    probe.expect(probe.sent(() => {
                        alert.handleKey(Qt.Key_Return);
                        alert.handleKey(Qt.Key_Enter);
                        alert.handleKey(Qt.Key_Escape);
                        alert.answer("allow");
                        alert.answer("deny");
                    }) === "[]", "input before armed was not ignored");
                    // Return is still held down when the alert arms (never released);
                    // Esc was pressed and released while it was not armed.
                    alert.keyReleased(Qt.Key_Escape);
                    probe.next();
                } else if (probe.phase === 1 && probe.elapsed() > 600) {
                    probe.expect(alert.armed === true, "armed after the delay");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_A)) === "[]",
                                 "other keys do nothing");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Return, true)) === "[]",
                                 "an auto-repeat does not answer");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Return, false)) === "[]",
                                 "a key held since before arming does not answer");
                    alert.keyReleased(Qt.Key_Return);
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Return, false))
                                 === '["old allow this_host/forever bindable=true"]',
                                 "a fresh Return press allows the oldest row");
                    probe.expect(alert.shown === false, "answered row is not shown again");
                    // The bridge decides it; the next row takes over, unarmed.
                    probe.decide("old", "allow");
                    alert.refresh();
                    probe.expect(alert.rowId === "new", "next row: " + alert.rowId);
                    probe.expect(alert.shown === true && alert.armed === false,
                                 "next row appears unarmed");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Escape)) === "[]",
                                 "Esc is ignored right after the switch");
                    alert.keyReleased(Qt.Key_Escape);
                    probe.next();
                } else if (probe.phase === 2 && probe.elapsed() > 600) {
                    probe.expect(alert.armed === true, "armed again after the switch");
                    // A host-scoped answer on a bridge without app-bound rules is once-only.
                    feedStub.appBound = false;
                    alert.refresh();
                    alert.scopeSelector.currentToken = "any_host_on_domain";
                    probe.expect(alert.remembers === false && alert.durationToken === "this_time",
                                 "old bridge: only Once");
                    // The offered durations vanished under the pointer: wait again.
                    probe.expect(alert.armed === false, "re-armed when the durations changed");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Escape)) === "[]",
                                 "Esc is ignored right after the durations changed");
                    alert.keyReleased(Qt.Key_Escape);
                    alert.armed = true;
                    probe.expect(alert.durationSelector.model.length === 1,
                                 "old bridge: one duration offered");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Escape))
                                 === '["new deny any_host_on_domain/this_time bindable=true"]',
                                 "Esc denies, once-only");
                    // "Any host" binds to the program alone, even on that bridge.
                    probe.decide("new", "deny");
                    probe.insert([probe.row("third", "/usr/bin/ssh", 3000)]);
                    alert.refresh();
                    alert.armed = true;
                    alert.scopeSelector.currentToken = "any_host";
                    probe.expect(alert.remembers === true && alert.durationToken === "forever",
                                 "any_host remembers: " + alert.durationToken);
                    probe.expect(alert.scopeToken === "any_host", "scope token");
                    // The default comes from Settings, per row.
                    Durations.defaultToken = "until_restart";
                    feedStub.appBound = true;
                    probe.decide("third", "allow");
                    probe.insert([probe.row("fourth", "/usr/bin/ssh", 4000)]);
                    alert.refresh();
                    probe.expect(alert.rowId === "fourth" && alert.scopeToken === "this_host"
                                 && alert.durationSelector.effectiveToken === "until_restart",
                                 "a new row resets to scope This host and the default duration");
                    // A row nobody can bind a rule to is once-only whatever is chosen.
                    probe.insert([probe.row("kernel", "Kernel connection", 5000)]);
                    probe.decide("fourth", "deny");
                    alert.refresh();
                    alert.armed = true;
                    probe.expect(alert.rowId === "kernel" && alert.bindable === false
                                 && alert.remembers === false, "unidentified program");
                    probe.expect(probe.sent(() => alert.handleKey(Qt.Key_Return))
                                 === '["kernel allow this_host/this_time bindable=false"]',
                                 "unidentified program is answered once");
                    // The connection to the bridge is gone: say so, stay up, re-arm.
                    probe.decide("kernel", "allow");
                    feedStub.queue = false;
                    probe.insert([probe.row("last", "/usr/bin/curl", 6000)]);
                    alert.refresh();
                    alert.armed = true;
                    probe.expect(alert.answer("allow") === false, "an unqueued verdict is reported");
                    probe.expect(alert.shown === true && alert.armed === false,
                                 "alert stays and re-arms when nothing was sent");
                    probe.expect(probe.explained.length === 1
                                 && probe.explained[0].indexOf("wasn't sent") > 0,
                                 "explained: " + JSON.stringify(probe.explained));
                    done = true;
                }
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("alert probe: " + probe.failures.join("; "));
                    }
                    console.warn("ALERT_WINDOW_PROBE_DONE");
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
        .filter(|l| l.contains(PROBE_URL) || l.contains("qml/AlertWindow.qml"))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(bad.is_empty(), "alert probe failed:\n{}", bad.join("\n"));
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}

/// The window's own key handler must call `handleKey` (the probe cannot press
/// a key), swallow every key, and the delay must be the specified 1000 ms.
#[test]
fn the_alert_wires_keys_to_the_armed_handler_and_waits_one_second() {
    assert!(
        ALERT_QML.contains("property int armDelayMs: 1000"),
        "the input delay is no longer 1000 ms"
    );
    let keys = ALERT_QML
        .split("Keys.onPressed")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("AlertWindow.qml has no Keys.onPressed");
    assert!(
        keys.contains("event.accepted = true")
            && keys.contains("alert.handleKey(event.key, event.isAutoRepeat)"),
        "Keys.onPressed must accept every key and call handleKey with isAutoRepeat: {keys}"
    );
    let released = ALERT_QML
        .split("Keys.onReleased")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("AlertWindow.qml has no Keys.onReleased");
    assert!(
        released.contains("alert.keyReleased(event.key)")
            && released.contains("!event.isAutoRepeat"),
        "a real release (not an auto-repeat) must be forgotten: {released}"
    );
    assert!(
        ALERT_QML.contains("onRemembersChanged: alert.arm()"),
        "the alert must re-arm when the offered durations change"
    );
    assert!(
        ALERT_QML.contains("enabled: alert.armed"),
        "clicks must be ignored until armed"
    );
    assert!(
        ALERT_QML.contains("Qt.Dialog | Qt.WindowStaysOnTopHint"),
        "the alert must be an always-on-top dialog"
    );
}

/// Program, path and host come from outside: every label that shows them is
/// PlainText (issue #51).
#[test]
fn the_alert_labels_showing_connection_data_are_plain_text() {
    for name in [
        "alertProgram",
        "alertPath",
        "alertDestination",
        "alertCountdown",
    ] {
        let block = ALERT_QML
            .split(&format!("objectName: \"{name}\""))
            .nth(1)
            .and_then(|rest| rest.split("Controls.").next())
            .unwrap_or_else(|| panic!("no {name} label"));
        assert!(
            block.contains("textFormat: Text.PlainText"),
            "{name} is not PlainText"
        );
    }
}
