//! End to end: the tray menu's real `MenuItem`s, triggered the way the
//! StatusNotifierItem tray triggers them, emit the `SetFilteringPaused`
//! message the bridge acts on (issue #47 follow-up).
//!
//! VM run r6 on real Plasma showed the timed-pause entries unreachable: the
//! 5/30/60 minute items sat in a nested `Labs.Menu` that the dbusmenu export
//! rendered as an empty "Pause filtering" entry. This probe loads the actual
//! `TrayMenu` type with a real `TrayController`, triggers every pause and
//! resume item, and checks the JSON `TrayController` emits for each — so a
//! miswired item, a wrong duration or a broken invokable fails here, not on
//! a user's tray.
//!
//! The probe reports each emitted JSON on stderr (`console.warn`, which the
//! headless logging setup keeps on stderr) and quits; the assertions run in
//! Rust. Same harness constraints as `bridge_feed_qml.rs`: a `Window` root
//! driven by `QGuiApplication::exec()` and a timer, and a stderr capture
//! that catches QML runtime errors. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::ws_messages::ClientMessage;

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/tray_menu_probe.qml";
const MARKER: &str = "TRAY_ACTION ";

#[test]
fn every_tray_pause_item_sends_its_timed_pause() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    // Items are found by objectName, not text, so this checks wiring rather
    // than labels. `finally` guarantees Qt.quit() even if a call throws.
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 400
    height: 300

    function raiseAndActivate() {}

    TrayController {
        id: controller
    }
    TrayMenu {
        id: trayMenu
        controller: controller
        window: probeWindow
    }
    Connections {
        target: controller
        function onFilteringToggleRequested(json) {
            console.warn("TRAY_ACTION " + json);
        }
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                for (const name of ["pauseFor300", "pauseFor1800", "pauseFor3600", "resumeFiltering"]) {
                    const item = trayMenu.items.find(i => i.objectName === name);
                    if (item === undefined) {
                        console.warn("TRAY_ACTION missing " + name);
                        continue;
                    }
                    item.triggered();
                }
            } finally {
                Qt.quit();
            }
        }
    }
}
"#;

    let root_ok = Arc::new(AtomicBool::new(false));
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
        // Without a root there is no timer to quit the event loop.
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(
        root_ok.load(Ordering::SeqCst),
        "tray menu probe did not load (TrayMenu or TrayController missing?); stderr:\n{captured}"
    );

    let errors: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) && !line.contains(MARKER))
        .collect();
    assert!(
        errors.is_empty(),
        "QML errors while driving the tray menu:\n{}",
        errors.join("\n")
    );

    let sent: Vec<String> = captured
        .lines()
        .filter_map(|line| line.split_once(MARKER).map(|(_, rest)| rest.to_owned()))
        .collect();
    let messages: Vec<ClientMessage> = sent
        .iter()
        .map(|json| {
            serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("tray item sent {json:?}, not a ClientMessage: {e}"))
        })
        .collect();
    let expected: Vec<ClientMessage> = [Some(300), Some(1800), Some(3600), None]
        .into_iter()
        .map(|duration_secs| ClientMessage::SetFilteringPaused {
            paused: duration_secs.is_some(),
            duration_secs,
            sender_generation: None,
            sender_uid: None,
        })
        .collect();
    assert_eq!(
        messages, expected,
        "tray items did not send the expected pause/resume messages; stderr:\n{captured}"
    );

    let _ = app.as_mut();
}
