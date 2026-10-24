//! Integration smoke: `GeoModel` is usable as a QML element (Geo panel).
//!
//! Mirrors `tests/connections_model_qml.rs`'s scope exactly, for the same
//! reasons documented there:
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile against its `QAbstractListModel` base), and
//!   * the `applyServerMessageJson` invokable is callable from QML and drives
//!     the real deserialize -> `GeoStore::apply` path without aborting.
//!
//! It intentionally does NOT assert row counts/content — the aggregate's
//! ordering/CRUD correctness is covered exhaustively and Qt-free by the
//! `geo::store` unit tests. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::geo_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn geo_model_registers_and_ingest_invokable_runs() {
    init_headless_qt_env();
    // Fedora's Qt logging configuration can suppress the positive marker.
    std::env::set_var("QT_LOGGING_RULES", "qml.warning=true");
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust model and drive one real insert through the
    // deserialize -> apply path. A panicking invokable would abort the whole
    // test binary; an unregistered type would leave the root object null.
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 500
    height: 320
    property GeoModel model: GeoModel {}
    GeoPage {
        id: geoPage
        anchors.fill: parent
        model: probeWindow.model
    }
    GeoDatabaseSettings {
        id: databaseSettings
        x: -1000
        width: parent.width
        model: probeWindow.model
    }
    function find(item, name) {
        if (item.objectName === name) return item;
        for (let child of item.children) {
            let found = find(child, name);
            if (found) return found;
        }
        return null;
    }
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "insertConnectionRows",
            rows: [
                { id: "r1", process: "firefox", processPath: null, dstHost: "github.com",
                  dstIp: "192.168.1.5", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0 },
                { id: "r2", process: "slack", processPath: null, dstHost: "slack.com",
                  dstIp: "8.8.8.8", dstPort: 443, protocol: "tcp", direction: "outgoing",
                  action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 0 }
            ]
        }));
        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] GeoModel.count after insert =", model.count,
                    "dbAvailable =", model.dbAvailable, "dbPath =", model.dbPath);
        if (!model.dbInstallPath.endsWith("/snitchwatch/GeoLite2-Country.mmdb"))
            throw new Error("Missing app-data install location");
        if (find(databaseSettings, "geoDatabasePath").text !== model.dbInstallPath)
            throw new Error("Install location is not displayed");
        if (!find(databaseSettings, "geoDownloadButton") || !find(databaseSettings, "geoInstallButton").enabled)
            throw new Error("Missing download or install button");
        model.dbAvailable = true;
        model.dbSource = "IP Geolocation by DB-IP · CC BY 4.0 · database build 1790818572 (Unix time)";
        if (find(databaseSettings, "geoDatabaseSource").text !== model.dbSource)
            throw new Error("Active database source attribution is not displayed");
        model.dbBuildEpoch = 1790818572;
        if (find(databaseSettings, "geoDatabaseBuild").text.indexOf("2026-10-01") < 0)
            throw new Error("Database build date is not displayed in UTC");
        for (let epoch of [0, -1, 18446744073709551615, Infinity, NaN]) {
            model.dbBuildEpoch = epoch;
            const build = find(databaseSettings, "geoDatabaseBuild");
            if (!build.visible || build.text !== "Database build date unknown.")
                throw new Error("Invalid or absent build epoch did not render as unknown");
        }
        model.installDatabase("https://example.invalid/database.mmdb");
        if (model.dbInstallBusy || model.dbInstallError.length === 0)
            throw new Error("Nonlocal file was not rejected");
        if (find(databaseSettings, "geoInstallError").text !== model.dbInstallError)
            throw new Error("Install error is not displayed");
        model.dbRestartRequired = true;
        if (!find(databaseSettings, "geoRestartNotice").visible)
            throw new Error("Restart notice is not displayed");
        model.dbAvailable = false;
        console.warn("[test] geo database controls passed");
    }
    Timer {
        interval: 100
        running: true
        onTriggered: {
            try {
                const view = geoPage.flickable;
                if (view.contentHeight <= view.height)
                    throw new Error("No-database setup does not participate in scrolling");
                for (const name of ["geoDownloadButton", "geoInstallButton", "geoInstallError", "geoRestartNotice"]) {
                    const item = find(geoPage, name);
                    if (!item || !item.visible)
                        throw new Error("Setup control or feedback missing: " + name);
                    const top = item.mapToItem(view.contentItem, 0, 0).y;
                    view.contentY = Math.max(view.originY, Math.min(top, view.originY + view.contentHeight - view.height));
                    const visibleTop = item.mapToItem(view, 0, 0).y;
                    if (visibleTop < -1 || visibleTop + item.height > view.height + 1)
                        throw new Error("Cannot reveal setup control or feedback at 500x320: " + name);
                }
                model.dbAvailable = true;
                const source = find(geoPage, "geoPageDatabaseSource");
                if (!source || !source.visible || source.text !== model.dbSource)
                    throw new Error("Active-view provider attribution disappeared");
                console.warn("[test] minimum-window geo setup scroll passed");
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
            engine.load_data(
                &QByteArray::from(qml),
                &QUrl::from("qrc:/inline_geo_model_probe.qml"),
            );
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
        "GeoModel QML probe failed: root object was null — the type failed to register as \
         a QML element or did not compile against its QAbstractListModel base"
    );
    assert!(
        captured.contains("[test] geo database controls passed"),
        "{captured}"
    );
    assert!(!captured.contains("Error:"), "{captured}");
    assert!(
        captured.contains("[test] minimum-window geo setup scroll passed"),
        "{captured}"
    );

    let _ = app.as_mut();
}
