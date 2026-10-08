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
const STATE_MARKER: &str = "TRAY_STATE ";

/// The pause items, by objectName, in menu order, with their duration and
/// text on a bridge that doesn't answer waiting prompts.
const PAUSE_ITEMS: [(&str, u64, &str); 3] = [
    ("pauseFor300", 300, "Pause for 5 minutes"),
    ("pauseFor1800", 1800, "Pause for 30 minutes"),
    ("pauseFor3600", 3600, "Pause for 1 hour"),
];

/// What each pause item adds when the connected bridge advertises
/// `pauseAnswersWaiting` (issue #78): a pause also lets the prompts already
/// waiting through once. A bridge that doesn't do that must not be promised.
const WAITING_SUFFIX: &str = " (also lets waiting connections through once)";

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

    // Every item, as the tray would render it, plus whether any item opens
    // a submenu (the nested-menu form the Plasma tray failed to export) and
    // whether the connected bridge answers waiting prompts on a pause.
    function snapshot(label) {
        return {
            label: label,
            answersWaiting: controller.pauseAnswersWaiting,
            submenus: trayMenu.items.filter(i => i.subMenu !== null).length,
            items: trayMenu.items.filter(i => i.objectName !== "").map(i => ({
                name: i.objectName,
                visible: i.visible,
                enabled: i.enabled,
                text: i.text
            }))
        };
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                // The menu exists from startup, before the first bridge state
                // ("default"); the tray may never re-read a visibility flip.
                // Nothing has said the bridge answers waiting prompts yet.
                console.warn("TRAY_STATE " + JSON.stringify(probeWindow.snapshot("startup")));
                for (const answersWaiting of [false, true]) {
                    controller.pauseAnswersWaiting = answersWaiting;
                    for (const [label, until] of [["default", ""], ["pause_filtering", ""],
                                                  ["resume_filtering", "14:30"], ["reconnect", ""]]) {
                        controller.menuLabel = label;
                        controller.pausedUntil = until;
                        console.warn("TRAY_STATE " + JSON.stringify(probeWindow.snapshot(label)));
                    }
                }
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
        .filter(|line| {
            line.contains(PROBE_URL) && !line.contains(MARKER) && !line.contains(STATE_MARKER)
        })
        .collect();
    assert!(
        errors.is_empty(),
        "QML errors while driving the tray menu:\n{}",
        errors.join("\n")
    );

    assert_menu_states(&captured);

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
    let expected: Vec<ClientMessage> = PAUSE_ITEMS
        .iter()
        .map(|&(_, secs, _)| Some(secs))
        .chain([None])
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

/// Every pause and resume item is always visible and flat; `enabled` and the
/// Resume text follow the bridge state. A live `visible` flip may never reach
/// an already-exported StatusNotifierItem menu, so nothing may depend on one.
fn assert_menu_states(captured: &str) {
    let states: Vec<serde_json::Value> = captured
        .lines()
        .filter_map(|line| line.split_once(STATE_MARKER).map(|(_, rest)| rest))
        .map(|json| serde_json::from_str(json).expect("probe state is JSON"))
        .collect();
    assert_eq!(
        states.len(),
        9,
        "probe reported {} states; stderr:\n{captured}",
        states.len()
    );

    for state in &states {
        let label = state["label"].as_str().unwrap();
        let answers_waiting = state["answersWaiting"].as_bool().unwrap_or_else(|| {
            panic!("{label}: the tray controller has no pauseAnswersWaiting property")
        });
        // Nothing but a bridge's acknowledgement may switch the promise on.
        assert!(
            label != "startup" || !answers_waiting,
            "a controller no bridge has spoken to promises the pause answers waiting prompts"
        );
        assert_eq!(state["submenus"], 0, "{label}: the tray menu has a submenu");
        let item = |name: &str| {
            state["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["name"] == name)
                .unwrap_or_else(|| panic!("{label}: no item {name}"))
                .clone()
        };
        let (can_pause, resume_text, can_resume) = match label {
            "pause_filtering" => (true, "Resume filtering", false),
            "resume_filtering" => (false, "Resume filtering (until 14:30)", true),
            // Not connected yet, or the daemon is unreachable.
            "startup" | "default" | "reconnect" => (false, "Resume filtering", false),
            other => panic!("unexpected state {other}"),
        };
        let suffix = if answers_waiting { WAITING_SUFFIX } else { "" };
        for (name, _, text) in PAUSE_ITEMS {
            let pause = item(name);
            assert_eq!(
                pause["text"],
                format!("{text}{suffix}"),
                "{label} (answers waiting: {answers_waiting}): {name} text"
            );
            assert_eq!(pause["visible"], true, "{label}: {name} hidden");
            assert_eq!(pause["enabled"], can_pause, "{label}: {name} enabled");
        }
        let resume = item("resumeFiltering");
        assert_eq!(resume["visible"], true, "{label}: Resume hidden");
        assert_eq!(resume["enabled"], can_resume, "{label}: Resume enabled");
        assert_eq!(resume["text"], resume_text, "{label}: Resume text");
    }
}
