//! Exercise the shipped QML helper and read back through native QClipboard,
//! without a second text document normalizing the captured path again.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[path = "../src/clipboard_native_test.rs"]
mod clipboard_native_ffi;
mod common;

#[test]
fn clipboard_copy_preserves_exact_native_plain_text() {
    common::init_headless_qt_env();
    let _app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: the pointer is only checked for null.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    for value in [
        "/opt/line\rbreak/app",
        "/opt/line\r\nbreak/app",
        "/opt/line\u{2028}break/app",
        "/opt/line\u{2029}break/app",
        "/opt/line\nbreak/app",
        "/opt/a\tb/app",
        "  /opt/space dir/app  ",
        "<b>captured & literal</b>",
        "2001:db8::1",
        "0",
        "nul\0byte",
        "",
    ] {
        root_ok.store(false, Ordering::SeqCst);
        let quoted = serde_json::to_string(value)
            .expect("encode QML string")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        let qml = format!(
            "import QtQuick\nimport com.snitchwatch.shell\n\
             ClipboardCopy {{ Component.onCompleted: copy({quoted}) }}"
        );
        let captured = common::capture_stderr(|| {
            engine.as_mut().expect("QML engine").load_data(
                &QByteArray::from(qml.as_str()),
                &QUrl::from("qrc:/clipboard_native_probe.qml"),
            );
        });
        assert!(root_ok.load(Ordering::SeqCst), "QML root: {captured}");
        assert!(
            !captured.contains("clipboard_native_probe.qml") && !captured.contains("TypeError"),
            "clipboard QML failed: {captured}"
        );
        assert_eq!(
            clipboard_native_ffi::ffi::clipboard_text().to_string(),
            value,
            "native clipboard changed captured text {value:?}"
        );
    }
    drop(guard);
}
