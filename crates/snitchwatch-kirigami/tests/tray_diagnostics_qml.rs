//! Q04: a tray started on a live bridge learns the firewall from the snapshot
//! its feed requests, and never sends `RecheckDiagnostics`, which clears the
//! bridge's stored daemon alerts (an alert that arrived after the health
//! model's own startup request would vanish). Fixture service, no daemon.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use tokio::net::UnixListener;

#[allow(unused_imports)]
use snitchwatch_kirigami::tray_controller as _;

mod common;
#[path = "support/tray_bridge_fixture.rs"]
mod tray_bridge_fixture;
use tray_bridge_fixture::{firewall_off, serve_session, TOKEN};

#[test]
fn a_tray_started_while_connected_sends_no_diagnostics_recheck() {
    common::init_headless_qt_env();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("bridge.sock");
    let token = dir.path().join("token");
    std::fs::write(&token, TOKEN).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    std::env::set_var("SNITCHWATCH_WS_SOCKET", &socket);
    std::env::set_var("SNITCHWATCH_WS_TOKEN_PATH", &token);

    let rechecks = Arc::new(AtomicUsize::new(0));
    let listener = runtime.block_on(async { UnixListener::bind(&socket).unwrap() });
    let server = runtime.spawn({
        let rechecks = rechecks.clone();
        async move {
            serve_session(
                &listener,
                firewall_off(),
                None,
                Duration::from_secs(30),
                &rechecks,
            )
            .await;
        }
    });
    snitchwatch_kirigami::bridge_runtime::ensure_started();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !snitchwatch_kirigami::bridge_runtime::handles().is_some_and(|h| h.is_connected()) {
        assert!(
            Instant::now() < deadline,
            "fixture bridge did not authenticate"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let created = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let created = created.clone();
        engine.on_object_created(move |_, obj, _| {
            created.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = common::capture_stderr(|| {
        engine.as_mut().unwrap().load_data(
            &QByteArray::from(PROBE),
            &QUrl::from("qrc:/tray_diagnostics_probe.qml"),
        );
        if created.load(Ordering::SeqCst) {
            app.as_mut().unwrap().exec();
        }
    });
    drop(guard);
    // Anything the tray sent has reached the fixture by now.
    std::thread::sleep(Duration::from_millis(500));
    server.abort();
    assert!(created.load(Ordering::SeqCst), "{captured}");
    assert!(captured.contains("TRAY_DIAGNOSTICS_OK"), "{captured}");
    assert_eq!(
        rechecks.load(Ordering::SeqCst),
        0,
        "the tray asked for a destructive diagnostics recheck"
    );
}

/// Started after the session is live; waits for the snapshot's firewall line.
const PROBE: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    visible: true
    width: 200
    height: 100

    TrayController { id: controller }
    property real started: Date.now()

    Component.onCompleted: controller.startBridgeFeed()

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            if (controller.tooltip.indexOf("Firewall not running as of opensnitchd's last connection") >= 0) {
                console.warn("TRAY_DIAGNOSTICS_OK");
                Qt.quit();
            } else if (Date.now() - started > 10000) {
                console.warn("Error: no firewall line from the snapshot: " + controller.tooltip);
                Qt.quit();
            }
        }
    }
}
"#;
