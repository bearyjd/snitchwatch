// Native-only test support; no QML type or application API is registered.
#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("accessibility.h");

        /// Turn Qt's accessibility layer on; whether it is now active.
        #[namespace = "snitchwatch::test"]
        #[rust_name = "activate_accessibility"]
        fn activateAccessibility() -> bool;
    }
}
