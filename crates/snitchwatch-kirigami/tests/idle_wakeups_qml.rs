//! perf/gui-idle-wakeups: while the window is hidden to the tray or
//! minimized, `main.qml`'s display-only timers stop (the 1 s banner tick and
//! the analysis-export dialog's 100 ms poll) and start again when it is shown.
//! Timers that answer, expire or end something (here the learning-window
//! expiry tick) keep running while hidden.
//!
//! Like `honest_ui_main_banner_qml.rs`, it never runs the event loop (under
//! one, `main.qml` hides to the tray and the test never quits): show/hide set
//! the window's visibility synchronously, and the `running` bindings follow.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/idle_wakeups_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

const PROBE: &str = r#"
import QtQuick

QtObject {
    function expect(what, actual, wanted) {
        if (actual !== wanted) {
            throw new Error(what + " is " + actual + ", wanted " + wanted);
        }
    }

    Component.onCompleted: {
        console.warn("IDLE_WAKEUPS_PROBE_START");
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        if (component.status !== Component.Ready) {
            throw new Error("main.qml: " + component.errorString());
        }
        const main = component.createObject(null, {});
        if (main === null) {
            throw new Error("main.qml was not created");
        }
        const tick = main.shownTickerRef;
        const exportPoll = main.analysisExport.pollTimer;
        const learning = main.alertHostRef.learningTimer;

        main.show();
        expect("shown: banner tick running", tick.running, true);
        expect("shown: export poll running", exportPoll.running, true);
        expect("shown: learning expiry running", learning.running, true);

        main.hide();
        expect("hidden: visibility", main.visibility, Window.Hidden);
        expect("hidden: banner tick running", tick.running, false);
        expect("hidden: export poll running", exportPoll.running, false);
        expect("hidden: learning expiry running", learning.running, true);

        main.show();
        expect("shown again: banner tick running", tick.running, true);
        expect("shown again: export poll running", exportPoll.running, true);

        main.visibility = Window.Minimized;
        expect("minimized: visibility", main.visibility, Window.Minimized);
        expect("minimized: banner tick running", tick.running, false);
        expect("minimized: export poll running", exportPoll.running, false);
        expect("minimized: learning expiry running", learning.running, true);

        main.showNormal();
        expect("restored: banner tick running", tick.running, true);
        main.hide();
        console.warn("IDLE_WAKEUPS_PROBE_DONE");
    }
}
"#;

#[test]
fn display_only_timers_stop_while_hidden_or_minimized_and_protected_ones_keep_running() {
    init_headless_qt_env();
    // Watchdog: no event loop runs here, but never let a stuck probe hang CI.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(240));
        eprintln!("idle_wakeups_qml: watchdog fired");
        std::process::exit(101);
    });

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

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
        }
    });
    drop(guard);

    assert!(root_ok.load(Ordering::SeqCst), "probe failed to load");
    assert!(
        captured.contains("IDLE_WAKEUPS_PROBE_DONE"),
        "the probe did not finish:\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "idle-wakeups probe failed or warned:\n{}",
        bad_lines.join("\n")
    );
    // Keep `app` alive until here (Qt requires a QGuiApplication for the engine).
    let _ = app.as_mut();
}
