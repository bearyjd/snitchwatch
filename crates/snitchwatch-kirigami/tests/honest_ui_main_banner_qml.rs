//! Issue #51 follow-up: `main.qml`'s bridge banner shows the bridge runtime's
//! status text (its own error messages) in a PlainText label, under a
//! fixed-text InlineMessage. This creates `main.qml`, puts markup in that text
//! and fails on any QML warning from the probe or the shell's own QML.
//!
//! Like `smoke.rs`, it never runs the event loop: under one, `main.qml` keeps
//! the test from quitting (it did before this change too). Creation and the
//! property change evaluate the banner's bindings synchronously, which is
//! what this checks. `honest_ui_external_text_guards.rs` checks the source.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/honest_ui_main_banner_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_bridge_banner_shows_markup_in_the_status_text_as_plain_text() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick

QtObject {
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        if (component.status !== Component.Ready) {
            throw new Error("main.qml: " + component.errorString());
        }
        const main = component.createObject(null, {});
        if (main === null) {
            throw new Error("main.qml was not created");
        }
        const status = "Bridge unavailable: <b>bold</b> <img src='https://example.invalid/x.png'> &amp;";
        main.bridgeFeedRef.ok = false;
        main.bridgeFeedRef.statusText = status;
        const label = main.bridgeStatusLabel;
        if (label.text !== status || label.textFormat !== Text.PlainText) {
            throw new Error("the banner shows '" + label.text + "' as format " + label.textFormat);
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
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "main banner probe failed to load (QML parse error)"
    );
    // `main.qml` creates its pages before a page stack holds them, which Qt
    // reports once per page outside an event loop. That predates this check
    // and says nothing about the banner.
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .filter(|line| {
            !line.ends_with("Created graphical object was not placed in the graphics scene.")
        })
        .collect();
    assert!(
        bad_lines.is_empty(),
        "QML warnings while main.qml showed markup in the bridge status:\n{}",
        bad_lines.join("\n")
    );
    // Keep `app` alive until here (Qt requires a QGuiApplication for the engine).
    let _ = app.as_mut();
}
