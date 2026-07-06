// THROWAWAY SPIKE integration test — proof point 4.
//
// This is a cargo *integration test* (its own compilation unit under tests/),
// linking against the kirigami_spike library. It is the exact shape of
// cxx-qt issue #770: if the cxx-qt-generated C++ / QML-registration object
// files do not get linked into a separate test target, this test binary
// fails to LINK (undefined references to the generated symbols).
//
// So the load-bearing signal here is simply: does `cargo test` compile, link
// and run this to completion? If yes, #770 does not bite this repo's tests/
// convention. Run headless with QT_QPA_PLATFORM=offscreen.
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

// Force the library — and therefore the cxx-qt generated bridge object files
// and the static QML-module registration — to be linked into this test binary.
#[allow(unused_imports)]
use kirigami_spike::cxxqt_object as _;

#[test]
fn cxx_qt_generated_code_links_into_tests_target() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    // Inline QML that imports the generated module and instantiates the Rust
    // QAbstractListModel subclass. loadData instantiates the tree immediately,
    // so Component.onCompleted runs synchronously. If the QML type were not
    // registered (i.e. the generated code did not link in), Qt prints a
    // "ConnectionModel is not a type" error; the test at minimum proves the
    // binary links and the engine runs.
    let qml = r#"
import QtQuick
import com.snitchwatch.spike

QtObject {
    property ConnectionModel model: ConnectionModel {}
    Component.onCompleted: {
        model.appendRow("probe-proc", "127.0.0.1:9");
    }
}
"#;

    if let (Some(engine), Some(_app)) = (engine.as_mut(), app.as_mut()) {
        engine.load_data(&QByteArray::from(qml), &QUrl::from("qrc:/inline_probe.qml"));
    }

    // Reaching here means the test target linked the cxx-qt generated code.
    assert!(engine.as_ref().is_some());
}
