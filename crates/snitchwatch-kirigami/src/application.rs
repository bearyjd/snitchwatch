//! QApplication factory for the Qt.labs.platform tray/widget fallback.
//!
//! cxx-qt-lib exposes QGuiApplication, so C++ owns the QApplication subclass
//! and returns its virtual base. Argument storage follows the pinned library's
//! ApplicationArgsData ownership contract.
use cxx_qt_lib::{QByteArray, QGuiApplication, QVector};

#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qguiapplication.h");
        type QGuiApplication = cxx_qt_lib::QGuiApplication;
        include!("cxx-qt-lib/core/qvector/qvector_QByteArray.h");
        type QVector_QByteArray = cxx_qt_lib::QVector<cxx_qt_lib::QByteArray>;
        include!("application.h");
        #[namespace = "snitchwatch"]
        #[rust_name = "new_widget_application"]
        fn newWidgetApplication(args: &QVector_QByteArray) -> UniquePtr<QGuiApplication>;
        #[namespace = "snitchwatch"]
        #[rust_name = "widget_application_smoke"]
        fn widgetApplicationSmoke(app: &QGuiApplication) -> bool;
    }
}

/// Construct the application's widget-capable Qt instance.
pub fn new() -> cxx::UniquePtr<QGuiApplication> {
    let mut args = QVector::<QByteArray>::default();
    for arg in std::env::args_os() {
        #[cfg(unix)]
        use std::os::unix::ffi::OsStrExt;
        #[cfg(windows)]
        let arg = arg.to_string_lossy();
        args.append(QByteArray::from(arg.as_bytes()));
    }
    ffi::new_widget_application(&args)
}

/// Used by the separate single-process widget regression test.
#[doc(hidden)]
pub fn widget_smoke() -> bool {
    let mut args = QVector::<QByteArray>::default();
    args.append(QByteArray::from("snitchwatch-application-probe".as_bytes()));
    args.append(QByteArray::from(
        "--snitchwatch-argv-lifetime-probe".as_bytes(),
    ));
    let app = ffi::new_widget_application(&args);
    drop(args);
    app.as_ref().is_some_and(ffi::widget_application_smoke)
}
