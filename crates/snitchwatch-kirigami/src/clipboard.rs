//! GUI-only plain-text writer; preserve the captured QString without routing
//! it through a text document, which normalizes some legal path characters.
use core::pin::Pin;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("clipboard.h");

        #[namespace = "snitchwatch"]
        #[rust_name = "set_clipboard_text"]
        fn setClipboardText(text: &QString);
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        type NativeClipboard = super::NativeClipboardRust;

        #[qinvokable]
        fn write(self: Pin<&mut NativeClipboard>, text: &QString);
    }
}

#[derive(Default)]
pub struct NativeClipboardRust;

impl qobject::NativeClipboard {
    fn write(self: Pin<&mut Self>, text: &QString) {
        qobject::set_clipboard_text(text);
    }
}
