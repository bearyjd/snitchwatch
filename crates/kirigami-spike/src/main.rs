// THROWAWAY SPIKE binary. Opens a Kirigami ApplicationWindow via cxx-qt +
// QML and exercises the async-signal + QAbstractListModel proof points.
//
// Runs headless-friendly: set QT_QPA_PLATFORM=offscreen to run without a
// display; the QML side auto-quits after the feed completes so the process
// exits cleanly in CI.
// Pull the bridge in through the library target so the bin and any
// integration tests share the same generated code path (keeps the static
// QML-type registration in the final link).
#[allow(unused_imports)]
use kirigami_spike::cxxqt_object as _;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

fn main() {
    // Kirigami works with QGuiApplication; pick a QtQuick.Controls style that
    // does not require a QApplication so this runs under `offscreen`.
    // `org.kde.desktop` (Kirigami's usual default) hangs indefinitely under
    // `QT_QPA_PLATFORM=offscreen` — confirmed by running this binary with no
    // other env vars set: exit 124, zero output, hangs before QML even
    // loads. `Basic` is the QApplication-free style that actually completes
    // headless (matches what `tests/link_770.rs` already sets).
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    if let Some(app) = app.as_mut() {
        app.set_application_name(&QString::from("kirigami-spike"));
    }

    if let Some(engine) = engine.as_mut() {
        // The QmlModule registers files under this canonical qrc path.
        engine.load(&QUrl::from(
            "qrc:/qt/qml/com/snitchwatch/spike/qml/main.qml",
        ));
    }

    if let Some(app) = app.as_mut() {
        std::process::exit(app.exec());
    }
}
