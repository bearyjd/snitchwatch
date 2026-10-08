//! Prompt-slot plan, part A (and issue #78), in the window: the real
//! `PromptSlotBanner.qml` with a stand-in `PromptSlotStatus`, a stub
//! `BridgeFeed` and a real `ConnectionsModel`.
//!   * It shows only for a supporting bridge, a held slot and a live feed.
//!   * The holder's text (program and host included) is plain text.
//!   * "Allow once" and "Deny" use the inline semantics, only while the model
//!     holds the row as pending, and answer each holder at most once.
//!   * A once-only Deny's explanation reaches the window.
//!
//! Fails on any QML warning from the probe or the shell's QML. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/prompt_slot_banner_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

/// Drop whole-line `//` comments.
fn code(source: &str) -> String {
    source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_heading_is_fixed_text_and_the_holder_is_plain_text() {
    let banner = code(include_str!("../qml/PromptSlotBanner.qml"));
    assert!(banner.contains("text: \"A connection is waiting for your answer.\""));
    assert_eq!(
        banner.matches("Kirigami.InlineMessage {").count(),
        1,
        "one InlineMessage, with fixed text"
    );
    let label = &banner[banner.find("id: holderLabel").expect("the holder label")..];
    assert!(
        label.contains("textFormat: Text.PlainText"),
        "the holder's program and host must be plain text (issue #51)"
    );
}

#[test]
fn the_age_based_estimate_is_only_for_bridges_without_the_prompt_slot() {
    let main = code(include_str!("../qml/main.qml"));
    let fallback = &main[main.find("id: pendingExposureBanner").unwrap()..];
    let visible = &fallback[fallback.find("visible:").unwrap()..];
    assert!(
        visible.starts_with("visible: !promptSlotStatus.supported"),
        "{}",
        &visible[..80]
    );
}

#[test]
fn the_banner_shows_the_holder_and_answers_it_once() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 800
    height: 600

    readonly property string markup: "<b>steam</b> → cdn.example has been waiting for your answer for 12 s."
    property var failures: []
    property var explanations: []

    QtObject {
        id: statusStub
        property bool supported: true
        property bool held: true
        property string rowId: "1:abs"
        property string text: probeWindow.markup
    }

    QtObject {
        id: feedStub
        property bool ok: true
        property var submitted: []
        function submitVerdict(rowId, choice, scope, duration) {
            feedStub.submitted.push(rowId + " " + choice + " " + scope + "/" + duration);
            return true;
        }
        function appBoundRulesFor(rowId) {
            return true;
        }
    }

    ConnectionsModel {
        id: connModel
    }

    PromptSlotBanner {
        id: banner
        anchors.left: parent.left
        anchors.right: parent.right
        status: statusStub
        model: connModel
        bridgeFeed: feedStub
        onExplained: text => probeWindow.explanations.push(text)
    }

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function row(id, processPath) {
        return { id: id, process: "p", processPath: processPath, dstHost: "example.com",
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                 matchedRule: null };
    }
    function sent(action) {
        feedStub.submitted = [];
        action();
        return JSON.stringify(feedStub.submitted);
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                connModel.applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        probeWindow.row("1:abs", "/usr/bin/curl"),
                        probeWindow.row("1:abs2", "/usr/bin/wget"),
                        probeWindow.row("1:kern", "Kernel connection")
                    ]
                }));

                probeWindow.check(banner.shown && banner.visible, "not shown");
                probeWindow.check(banner.label.text === probeWindow.markup
                                  && banner.label.textFormat === Text.PlainText,
                                  "label " + banner.label.text);

                probeWindow.check(probeWindow.sent(() => banner.answer("allow"))
                                  === JSON.stringify(["1:abs allow this_host/this_time"]),
                                  "Allow once sent " + JSON.stringify(feedStub.submitted));
                probeWindow.check(probeWindow.sent(() => banner.answer("deny")) === "[]",
                                  "a second answer for the same holder was sent");

                statusStub.rowId = "1:abs2";
                probeWindow.check(probeWindow.sent(() => banner.answer("deny"))
                                  === JSON.stringify(["1:abs2 deny this_host/until_quit"]),
                                  "Deny sent " + JSON.stringify(feedStub.submitted));

                statusStub.rowId = "1:kern";
                banner.answer("deny");
                probeWindow.check(probeWindow.explanations.length === 1
                                  && probeWindow.explanations[0].indexOf("couldn't identify") >= 0,
                                  "explanations " + JSON.stringify(probeWindow.explanations));

                statusStub.rowId = "1:gone";
                probeWindow.check(!banner.actionable, "actionable for a row the model lacks");
                probeWindow.check(probeWindow.sent(() => banner.answer("allow")) === "[]",
                                  "answered a row the model lacks");

                statusStub.rowId = "1:abs";
                statusStub.supported = false;
                probeWindow.check(!banner.shown, "shown for an older bridge");
                statusStub.supported = true;
                statusStub.held = false;
                probeWindow.check(!banner.shown, "shown with a free slot");
                statusStub.held = true;
                feedStub.ok = false;
                probeWindow.check(!banner.shown, "shown while disconnected");
                feedStub.ok = true;
                probeWindow.check(banner.shown, "not shown again");

                if (probeWindow.failures.length > 0) {
                    throw new Error("prompt slot banner probe: " + probeWindow.failures.join("; "));
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
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "prompt slot banner probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "prompt slot banner probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
