//! Rust `QObject` types exposed to the QML/Kirigami UI via cxx-qt.
//!
//! This is the boundary the plan calls out: `web/`'s JS state logic is
//! re-homed into Rust `QObject`s here and consumed directly by `.qml` files —
//! no WebSocket/JSON round-trip. Phase 3b grows this module task-by-task; for
//! now it carries the scaffold-level `AppInfo` object (Task 5).

/// Static build/app identity surfaced to the About screen and window title.
/// Kept in Rust (not hardcoded in QML) so it can track the crate version.
#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Read-only application identity, registered as a QML singleton-like
        /// element. Bound by `main.qml` for the window title / About row.
        #[qobject]
        #[qml_element]
        #[qproperty(QString, app_name, cxx_name = "appName")]
        #[qproperty(QString, version)]
        type AppInfo = super::AppInfoRust;
    }
}

/// Rust-side backing state for [`qobject::AppInfo`].
pub struct AppInfoRust {
    app_name: cxx_qt_lib::QString,
    version: cxx_qt_lib::QString,
}

impl Default for AppInfoRust {
    fn default() -> Self {
        Self {
            app_name: cxx_qt_lib::QString::from("Snitchwatch"),
            version: cxx_qt_lib::QString::from(env!("CARGO_PKG_VERSION")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_default_carries_name_and_crate_version() {
        let info = AppInfoRust::default();
        assert_eq!(info.app_name.to_string(), "Snitchwatch");
        assert_eq!(info.version.to_string(), env!("CARGO_PKG_VERSION"));
    }
}
