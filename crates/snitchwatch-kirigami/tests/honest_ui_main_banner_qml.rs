//! Issue #51 follow-up: `main.qml`'s bridge banner shows the bridge runtime's
//! status text (its own error messages) in a PlainText label, under a
//! fixed-text InlineMessage. This creates `main.qml`, puts markup in that text
//! and fails on any QML warning from the probe or the shell's own QML.
//!
//! The banner's sentence comes from the link state the Rust runtime reports
//! (`BridgeFeed.linkState`: connecting, retrying, failed, stopped), never from
//! that text, so it is right in every state: no "keeps retrying" while the
//! client is only starting, and none after it gave up.
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
const STATES_PROBE_URL: &str = "qrc:/honest_ui_main_banner_states_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

/// One `QGuiApplication` per process, so both probes share it and the engine.
#[test]
fn the_bridge_banner_is_plain_text_and_says_the_right_thing_in_every_link_state() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let mut loaded = Vec::new();
    let captured = capture_stderr(|| {
        if let Some(mut engine) = engine.as_mut() {
            for (qml, url) in [(MARKUP_PROBE, PROBE_URL), (STATES_PROBE, STATES_PROBE_URL)] {
                root_ok.store(false, Ordering::SeqCst);
                engine
                    .as_mut()
                    .load_data(&QByteArray::from(qml), &QUrl::from(url));
                loaded.push((url, root_ok.load(Ordering::SeqCst)));
            }
        }
    });
    drop(guard);

    for (url, ok) in loaded {
        assert!(ok, "banner probe {url} failed to load (QML parse error)");
    }
    // `main.qml` creates its pages before a page stack holds them, which Qt
    // reports once per page outside an event loop. That predates this check
    // and says nothing about the banner.
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains(STATES_PROBE_URL)
                || line.contains(SHELL_QML_PREFIX)
        })
        .filter(|line| {
            !line.ends_with("Created graphical object was not placed in the graphics scene.")
        })
        .collect();
    assert!(
        bad_lines.is_empty(),
        "bridge banner probes failed or warned:\n{}",
        bad_lines.join("\n")
    );
    // Keep `app` alive until here (Qt requires a QGuiApplication for the engine).
    let _ = app.as_mut();
}

/// Markup in the bridge's status text shows as plain text in the banner.
const MARKUP_PROBE: &str = r#"
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

/// Each link state gets its own fixed sentence, whatever the status text says
/// and wherever `ok` is.
const STATES_PROBE: &str = r#"
import QtQuick

QtObject {
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        if (component.status !== Component.Ready) {
            throw new Error("main.qml: " + component.errorString());
        }
        const main = component.createObject(null, {});
        if (main === null) {
            throw new Error("main.qml was not created");
        }
        const sentences = {
            connecting: "Snitchwatch is connecting to its background service.",
            retrying: "Snitchwatch can't reach its background service. It keeps retrying.",
            failed: "Snitchwatch couldn't start its connection to the background service. Restart Snitchwatch to try again.",
            stopped: "Snitchwatch's connection to its background service has ended. Restart Snitchwatch to connect again."
        };
        // Text that would pick the wrong sentence if the banner read it.
        const misleading = "Bridge client stopped. Connected to bridge service. keeps retrying";
        for (const state in sentences) {
            main.bridgeFeedRef.ok = false;
            main.bridgeFeedRef.statusText = misleading;
            main.bridgeFeedRef.linkState = state;
            const shown = [];
            for (const other in sentences) {
                const message = find(main.contentItem, "bridgeMessage-" + other);
                if (message === null) {
                    throw new Error("no banner message for " + other);
                }
                if (message.visible) {
                    shown.push(other + ": " + message.text);
                }
            }
            if (shown.length !== 1 || shown[0] !== state + ": " + sentences[state]) {
                throw new Error("link state " + state + " shows " + JSON.stringify(shown));
            }
        }
        // Connected: no banner at all.
        main.bridgeFeedRef.ok = true;
        main.bridgeFeedRef.statusText = "Connected to bridge service";
        main.bridgeFeedRef.linkState = "connected";
        for (const other in sentences) {
            if (find(main.contentItem, "bridgeMessage-" + other).visible) {
                throw new Error("the banner shows " + other + " while connected");
            }
        }
    }
}
"#;
