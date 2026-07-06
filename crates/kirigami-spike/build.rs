// THROWAWAY SPIKE build script.
//
// Proves cxx-qt-build's build.rs (which drives moc/rcc/qmlcachegen and C++
// codegen) can run inside this Cargo workspace next to snitchwatch-proto's
// tonic-build and snitchwatch-tauri's tauri-build.
use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("com.snitchwatch.spike").qml_files(["qml/main.qml"]),
    )
    .file("src/cxxqt_object.rs")
    .build();
}
