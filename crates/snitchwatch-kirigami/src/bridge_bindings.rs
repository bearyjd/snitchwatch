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
        /// `SNITCHWATCH_DEMO=1`: fake rows, no bridge (see `crate::demo`).
        #[qproperty(bool, demo_mode, cxx_name = "demoMode")]
        /// Screenshot output directory; empty unless demo mode is on.
        #[qproperty(QString, screenshot_dir, cxx_name = "screenshotDir")]
        /// `SNITCHWATCH_TEXT_TRACE=1`: log on-screen text changes (`TextTrace.qml`).
        #[qproperty(bool, text_trace, cxx_name = "textTrace")]
        type AppInfo = super::AppInfoRust;
    }
}

/// Rust-side backing state for [`qobject::AppInfo`].
pub struct AppInfoRust {
    app_name: cxx_qt_lib::QString,
    version: cxx_qt_lib::QString,
    demo_mode: bool,
    screenshot_dir: cxx_qt_lib::QString,
    text_trace: bool,
}

/// Env var that turns the on-screen text change log on (exactly `1`).
pub const ENV_TEXT_TRACE: &str = "SNITCHWATCH_TEXT_TRACE";

fn text_trace_flag(value: Option<&str>) -> bool {
    value == Some("1")
}

impl Default for AppInfoRust {
    fn default() -> Self {
        Self {
            app_name: cxx_qt_lib::QString::from("Snitchwatch"),
            version: cxx_qt_lib::QString::from(env!("CARGO_PKG_VERSION")),
            demo_mode: crate::demo::enabled(),
            screenshot_dir: cxx_qt_lib::QString::from(
                crate::demo::screenshot_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default(),
            ),
            text_trace: text_trace_flag(std::env::var(ENV_TEXT_TRACE).ok().as_deref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exactly_one_enables_text_trace() {
        assert!(text_trace_flag(Some("1")));
        for v in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            assert!(!text_trace_flag(v), "{v:?}");
        }
    }

    #[test]
    fn app_info_default_carries_name_and_crate_version() {
        let info = AppInfoRust::default();
        assert_eq!(info.app_name.to_string(), "Snitchwatch");
        assert_eq!(info.version.to_string(), env!("CARGO_PKG_VERSION"));
    }
}
