//! A second launch's raise reaches the window through a real
//! `NotificationController`: a raise that arrives before QML exists (as an
//! `Activate` during startup does) is delivered once `startRaiseFeed()` runs,
//! as `raiseRequested`, exactly once. No bus here: the D-Bus side is tested
//! on a private bus in `src/single_instance/tests.rs`; `main.qml`'s handler
//! is checked by `qml_source_guards.rs` (`a_second_launch_raises_the_main_window`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::notification_controller as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: root
    visible: false
    width: 200
    height: 100
    property int raised: 0

    NotificationController { id: notificationController }

    // The same steps as main.qml's raiseAndActivate().
    function raiseAndActivate() {
        if (root.visibility === Window.Minimized || root.visibility === Window.Hidden)
            root.showNormal();
        root.raise();
        root.requestActivate();
    }

    Connections {
        target: notificationController
        function onRaiseRequested() {
            root.raised += 1;
            root.raiseAndActivate();
            settle.start();
        }
    }

    // Long enough for a second, wrong delivery to arrive too.
    Timer {
        id: settle
        interval: 500
        onTriggered: {
            if (root.raised === 1 && root.visible)
                console.info("RAISE_OK");
            else
                console.error("RAISE_BAD raised=" + root.raised + " visible=" + root.visible);
            Qt.quit();
        }
    }
    Timer { interval: 20000; running: true; onTriggered: { console.error("WATCHDOG"); Qt.quit(); } }

    Component.onCompleted: notificationController.startRaiseFeed()
}
"#;

#[test]
fn an_early_raise_is_delivered_to_the_window_once() {
    init_headless_qt_env();
    // Before the window exists: held as pending, like an Activate during startup.
    snitchwatch_kirigami::single_instance::raise_sink().raise();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let created = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let created = created.clone();
        engine.on_object_created(move |_, obj, _| {
            // SAFETY: pointer only tested for null, never dereferenced.
            created.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(PROBE),
                &QUrl::from("qrc:/single_instance_raise_probe.qml"),
            );
        }
        if created.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(created.load(Ordering::SeqCst), "{captured}");
    assert!(captured.contains("RAISE_OK"), "{captured}");
    assert!(!captured.contains("Error:"), "{captured}");
}
