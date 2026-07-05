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
    CxxQtBuilder::new_qml_module(
        QmlModule::new("com.snitchwatch.shell").qml_files(["qml/main.qml"]),
    )
    .file("src/bridge_bindings.rs")
    .build();
}
