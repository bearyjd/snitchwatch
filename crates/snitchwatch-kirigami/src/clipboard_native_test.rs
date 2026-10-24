#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("clipboard_native.h");

        #[namespace = "snitchwatch::test"]
        #[rust_name = "clipboard_text"]
        fn clipboardText() -> QString;
    }
}
