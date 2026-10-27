//! Q04 lifecycle: a real `TrayController` against a fixture bridge service
//! (no live daemon) through bridge unavailable, connected, a firewall-off
//! report, a disconnect and a reconnect. The pure `tray`/`tray_health` tests
//! can't see what the controller renders between those events:
//! - unavailable: the waiting tooltip and no pause offer, not the default Idle;
//! - a disconnect drops the firewall line and its time;
//! - no `RecheckDiagnostics` (`tray_diagnostics_qml.rs` checks it while
//!   connected at start; here the bridge is unavailable then).
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::ws_messages::CheckStatus;
use tokio::net::UnixListener;

#[allow(unused_imports)]
use snitchwatch_kirigami::tray_controller as _;

mod common;
#[path = "support/tray_bridge_fixture.rs"]
mod tray_bridge_fixture;
use tray_bridge_fixture::{firewall_off, serve_session, TOKEN};

#[test]
fn the_tray_follows_the_bridge_session_lifecycle() {
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
    let server = runtime.spawn({
        let rechecks = rechecks.clone();
        async move {
            // Unavailable first: nothing listens until the probe has looked.
            tokio::time::sleep(Duration::from_millis(2500)).await;
            let listener = UnixListener::bind(&socket).unwrap();
            serve_session(
                &listener,
                CheckStatus::Unknown,
                Some(firewall_off()),
                Duration::from_secs(4),
                &rechecks,
            )
            .await;
            // Disconnected long enough for the probe to see it.
            drop(listener);
            let _ = std::fs::remove_file(&socket);
            tokio::time::sleep(Duration::from_millis(2500)).await;
            let listener = UnixListener::bind(&socket).unwrap();
            serve_session(
                &listener,
                firewall_off(),
                None,
                Duration::from_secs(20),
                &rechecks,
            )
            .await;
        }
    });
    snitchwatch_kirigami::bridge_runtime::ensure_started();

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
            &QUrl::from("qrc:/tray_lifecycle_probe.qml"),
        );
        if created.load(Ordering::SeqCst) {
            app.as_mut().unwrap().exec();
        }
    });
    drop(guard);
    server.abort();
    assert!(created.load(Ordering::SeqCst), "{captured}");
    assert!(captured.contains("TRAY_LIFECYCLE_OK"), "{captured}");
    assert!(!captured.contains("Error:"), "{captured}");
    assert_eq!(
        rechecks.load(Ordering::SeqCst),
        0,
        "the tray asked for a destructive diagnostics recheck"
    );
}

/// Steps the controller must pass through, in order, each within its own
/// deadline. `unavailable` is held for a while: an Idle default that slips
/// in during startup must fail it.
const PROBE: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 200
    height: 100

    TrayController { id: controller }

    readonly property string waiting: "Snitchwatch — waiting for the bridge. Process monitor: not reported by opensnitchd"
    readonly property string firewallOff: "Firewall not running as of opensnitchd's last connection"
    property var steps: [
        { name: "unavailable", holdMs: 1500, ok: () =>
            controller.tooltip === waiting && controller.menuLabel === "default" },
        { name: "connected", ok: () =>
            controller.tooltip === "Snitchwatch — connected, prompts not paused. Process monitor: not reported by opensnitchd"
            && controller.menuLabel === "pause_filtering" },
        { name: "firewall off noticed", ok: () =>
            controller.tooltip.indexOf(firewallOff + " (noticed ") >= 0
            && controller.menuLabel === "pause_filtering" },
        { name: "disconnected", ok: () =>
            controller.tooltip === waiting && controller.menuLabel === "default" },
        { name: "reconnected, time unknown", ok: () =>
            controller.tooltip.indexOf(firewallOff + " (time not known)") >= 0
            && controller.menuLabel === "pause_filtering" }
    ]
    property int step: 0
    property real stepStarted: Date.now()
    property real heldSince: 0

    Component.onCompleted: controller.startBridgeFeed()

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            const current = probe.steps[probe.step];
            const now = Date.now();
            if (current.holdMs) {
                // Must hold from the start for holdMs.
                if (!current.ok()) {
                    console.warn("Error: " + current.name + ": " + controller.tooltip + " / " + controller.menuLabel);
                    Qt.quit();
                    return;
                }
                if (now - probe.stepStarted < current.holdMs) return;
            } else if (!current.ok()) {
                if (now - probe.stepStarted > 10000) {
                    console.warn("Error: timed out at " + current.name + ": " + controller.tooltip + " / " + controller.menuLabel);
                    Qt.quit();
                }
                return;
            }
            probe.step += 1;
            probe.stepStarted = now;
            if (probe.step === probe.steps.length) {
                console.warn("TRAY_LIFECYCLE_OK");
                Qt.quit();
            }
        }
    }
}
"#;
