//! Snitchwatch Kirigami shell — desktop entry point.
//!
//! Boots a `QGuiApplication`, registers the cxx-qt QML module (linked in via
//! the library target), and loads the Kirigami `ApplicationWindow` entry
//! point. This is the native replacement for `snitchwatch-tauri`'s
//! WebKitGTK-webview shell; it consumes `snitchwatch-bridge`'s typed Rust APIs
//! directly rather than round-tripping JSON over a WebSocket to itself.

// Pull the cxx-qt bridge in through the library target so the bin and any
// integration tests share the same generated code path — this keeps the
// static QML-type registration (`AppInfo`, and future models) in the final
// link. Without this `use`, the linker drops the generated module and QML
// fails with "AppInfo is not a type".
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};
use snitchwatch_kirigami::bridge_runtime;

fn main() {
    // Kirigami's usual `org.kde.desktop` style needs a real Plasma session and
    // hangs under `QT_QPA_PLATFORM=offscreen`; `Basic` is the QApplication-free
    // style that completes headless (matches the spike + tests). Only default
    // it when the operator hasn't chosen a style, so a real desktop session
    // still gets native Plasma styling.
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none()
        && std::env::var_os("QT_QPA_PLATFORM").as_deref() == Some(std::ffi::OsStr::new("offscreen"))
    {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    // Start the in-process bridge before loading QML so the models' live feeds
    // find it already running when their `Component.onCompleted` fires. Startup
    // failure is non-fatal: the window still opens and the error is surfaced in
    // the UI via the `BridgeFeed` status property (bound to a Kirigami
    // InlineMessage in main.qml). Never panic here — a failed bridge must
    // degrade to a visible status, not a dead window.
    match bridge_runtime::ensure_started() {
        (true, msg) => tracing::info!(status = %msg, "in-process bridge started"),
        (false, msg) => tracing::error!(status = %msg, "in-process bridge unavailable"),
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(app) = app.as_mut() {
        app.set_application_name(&QString::from("Snitchwatch"));
    }

    if let Some(engine) = engine.as_mut() {
        // The QmlModule registers files under this canonical qrc path.
        engine.load(&QUrl::from(
            "qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml",
        ));
    }

    if let Some(app) = app.as_mut() {
        std::process::exit(app.exec());
    }
}
