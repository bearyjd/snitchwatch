// Native-only test support; no QML type or application API is registered.
#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("key_repeat.h");

        /// Make a key sent with `Qt.GroupSwitchModifier` arrive as an
        /// auto-repeat; whether the filter is installed.
        #[namespace = "snitchwatch::test"]
        #[rust_name = "install_auto_repeat_keys"]
        fn installAutoRepeatKeys() -> bool;
    }
}
