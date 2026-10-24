// Native-only test support; no QML type or application API is registered.
#[cxx::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
        include!("delegate_incubation.h");

        #[namespace = "snitchwatch::test"]
        type IncubationHold;
        #[namespace = "snitchwatch::test"]
        #[rust_name = "hold_incubation"]
        fn holdIncubation(engine: Pin<&mut QQmlApplicationEngine>) -> UniquePtr<IncubationHold>;
        #[rust_name = "max_pending"]
        fn maxPending(self: &IncubationHold) -> i32;
    }
}
