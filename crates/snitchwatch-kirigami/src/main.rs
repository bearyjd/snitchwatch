//! Snitchwatch Kirigami shell — desktop entry point.
//!
//! Boots a `QApplication`, registers the cxx-qt QML module (linked in via
//! the library target), and loads the Kirigami `ApplicationWindow` entry
//! point. This is the native replacement for `snitchwatch-tauri`'s
//! WebKitGTK-webview shell; it consumes the separately managed bridge service
//! over its authenticated Unix-domain WebSocket.

// Pull the cxx-qt bridge in through the library target so the bin and any
// integration tests share the same generated code path — this keeps the
// static QML-type registration (`AppInfo`, and future models) in the final
// link. Without this `use`, the linker drops the generated module and QML
// fails with "AppInfo is not a type".
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

use cxx_qt_lib::{QQmlApplicationEngine, QString, QUrl};
use snitchwatch_kirigami::{
    app_icons, application, bridge_runtime, demo, logging, notification_signals, panic_hook, paths,
};

fn main() {
    if let Some(code) = snitchwatch_kirigami::geo::install::validation_worker_entry() {
        std::process::exit(code);
    }
    // Without this, every `tracing::info!`/`tracing::error!` call in this
    // crate (including the bridge-startup log below, and any QML handler
    // diagnostics) is silently dropped: the default `tracing` dispatcher is
    // a no-op until a subscriber is installed. `SNITCHWATCH_LOG` (or
    // `RUST_LOG`) sets the filter; see `logging` for where it goes.
    logging::init();

    // The Diagnostics page reads this file; nothing wrote it until now.
    panic_hook::install(paths::crash_log_path());

    // Index the host's `.desktop` files off the UI thread, so it is ready by
    // the time rows ask for icons.
    app_icons::prewarm();

    // Kirigami's usual `org.kde.desktop` style needs a real Plasma session and
    // hangs under `QT_QPA_PLATFORM=offscreen`; `Basic` is the lightweight
    // style that completes headless (matches the spike + tests). Only default
    // it when the operator hasn't chosen a style, so a real desktop session
    // still gets native Plasma styling.
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none()
        && std::env::var_os("QT_QPA_PLATFORM").as_deref() == Some(std::ffi::OsStr::new("offscreen"))
    {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    // Start the bridge *client* before loading QML so the models' live feeds
    // find it already running when their `Component.onCompleted` fires. Startup
    // failure is non-fatal: the window still opens and the error is surfaced in
    // the UI via the `BridgeFeed` status property (bound to a Kirigami
    // InlineMessage in main.qml). Never panic here — a failed bridge must
    // degrade to a visible status, not a dead window.
    if demo::enabled() {
        // Demo mode never starts or contacts a bridge.
        tracing::info!("SNITCHWATCH_DEMO=1: demo mode, bridge client not started");
    } else {
        match bridge_runtime::ensure_started() {
            (true, msg) => tracing::info!(status = %msg, "bridge client started"),
            (false, msg) => tracing::error!(status = %msg, "bridge client unavailable"),
        }
    }

    let mut app = application::new();
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
        let code = app.exec();
        // What learning mode saw in the last moments is still in memory (a
        // burst is saved a couple of seconds after it starts).
        snitchwatch_kirigami::learned::flush_and_wait(std::time::Duration::from_secs(3));
        // Resident notices would outlive us, with buttons nobody hears.
        if let Some(handles) = bridge_runtime::handles() {
            handles
                .runtime()
                .block_on(notification_signals::close_all_open());
        }
        std::process::exit(code);
    }
}
