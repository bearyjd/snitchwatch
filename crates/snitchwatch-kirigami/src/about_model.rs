//! `AboutModel`: the About page's rows and copyable text, from `crate::about`.
//! Display only: it reads `/.flatpak-info` (path overridable for tests) when
//! QML asks it to refresh, and never writes or runs anything. The daemon's version is an input because only QML has it.

use core::pin::Pin;
use std::path::Path;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;

use crate::about::{fields, load_flatpak_state, report_text, BuildInfo, FLATPAK_INFO_PATH};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("qt_runtime.h");

        /// `qVersion()`: the Qt this process is running against.
        #[namespace = "snitchwatch"]
        #[rust_name = "qt_runtime_version"]
        fn qtRuntimeVersion() -> QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        /// The Flatpak instance file read on `refresh()`.
        #[qproperty(QString, flatpak_info_path, cxx_name = "flatpakInfoPath")]
        /// Input: the running Qt version; defaults to `qVersion()`.
        #[qproperty(QString, qt_version, cxx_name = "qtVersion")]
        /// Input: the connected daemon's version string.
        #[qproperty(QString, daemon_version, cxx_name = "daemonVersion")]
        /// Input: whether a daemon version has been reported.
        #[qproperty(bool, daemon_connected, cxx_name = "daemonConnected")]
        /// Output: JSON array of `{key, label, value}` rows.
        #[qproperty(QString, fields_json, cxx_name = "fieldsJson")]
        /// Output: the rows as plain text for the clipboard.
        #[qproperty(QString, report_text, cxx_name = "reportText")]
        type AboutModel = super::AboutModelRust;

        /// Re-read the Flatpak file and rebuild the outputs.
        #[qinvokable]
        fn refresh(self: Pin<&mut AboutModel>);
    }
}

/// Rust-side state for [`qobject::AboutModel`].
pub struct AboutModelRust {
    flatpak_info_path: QString,
    qt_version: QString,
    daemon_version: QString,
    daemon_connected: bool,
    fields_json: QString,
    report_text: QString,
}

impl Default for AboutModelRust {
    fn default() -> Self {
        Self {
            flatpak_info_path: QString::from(FLATPAK_INFO_PATH),
            qt_version: qobject::qt_runtime_version(),
            daemon_version: QString::default(),
            daemon_connected: false,
            fields_json: QString::from("[]"),
            report_text: QString::default(),
        }
    }
}

impl qobject::AboutModel {
    fn refresh(mut self: Pin<&mut Self>) {
        let path = self.rust().flatpak_info_path.to_string();
        let qt_version = self.rust().qt_version.to_string();
        let daemon_version = self.rust().daemon_version.to_string();
        let daemon = self
            .rust()
            .daemon_connected
            .then_some(daemon_version.as_str());
        let rows = fields(
            &BuildInfo::current(),
            &load_flatpak_state(Path::new(&path)),
            &qt_version,
            daemon,
        );
        let json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
        self.as_mut().set_fields_json(QString::from(&json));
        self.as_mut()
            .set_report_text(QString::from(&report_text(&rows)));
    }
}
