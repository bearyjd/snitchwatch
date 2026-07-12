//! cxx-qt build script for the Snitchwatch Kirigami shell.
//!
//! Drives moc/rcc/qmlcachegen and the cxx-qt C++/QML-registration codegen.
//! The QML module is registered under the canonical `com.snitchwatch.shell`
//! URI; `main.rs` loads its entry point from the generated `qrc:/` path.
//!
//! Rust `#[cxx_qt::bridge]` modules that back QML types are added via
//! `.file(...)` so their generated code is compiled and statically registered.
use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("com.snitchwatch.shell").qml_files([
        "qml/main.qml",
        "qml/ConnectionsPage.qml",
        "qml/PendingDecisionSheet.qml",
        "qml/BlocklistsPage.qml",
        "qml/RulesPage.qml",
        "qml/ProfilesPage.qml",
        "qml/TrafficPage.qml",
        "qml/OnboardingPage.qml",
        "qml/DiagnosticsPage.qml",
        "qml/GeoPage.qml",
        "qml/ScannerPage.qml",
    ]))
    .file("src/bridge_bindings.rs")
    .file("src/bridge_feed.rs")
    .file("src/connections_model.rs")
    .file("src/pending_decision.rs")
    .file("src/insight_model.rs")
    .file("src/blocklists_model.rs")
    .file("src/rules_model.rs")
    .file("src/profiles_model.rs")
    .file("src/traffic_model.rs")
    .file("src/wizard_controller.rs")
    .file("src/settings_controller.rs")
    .file("src/notification_controller.rs")
    .file("src/tray_controller.rs")
    .file("src/geo_model.rs")
    .file("src/scanner_controller.rs")
    .build();
}
