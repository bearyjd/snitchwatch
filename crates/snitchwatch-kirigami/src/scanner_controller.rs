//! `ScannerController` — Phase 6 report UI, bound by `ScannerPage.qml`.
//!
//! Wraps `crate::scanner::run_deep_scan` the same way `SettingsController`
//! wraps its filesystem calls: off the Qt thread via `std::thread::spawn`,
//! results delivered back through `CxxQtThread::queue`. `reportJson` is
//! handed to QML as a raw string rather than modeled field-by-field as
//! qproperties — this is a point-in-time report a human reads once per scan,
//! not a live stream needing incremental model updates the way
//! `ConnectionsModel` does, so QML parsing it with `JSON.parse` is the
//! simpler, correctly-scoped choice.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Phase 6 report UI surface: triggers a privileged deep scan and
        /// exposes its JSON report.
        #[qobject]
        #[qml_element]
        /// Raw `--json` stdout from the last successful scan, or empty
        /// before the first run. Parsed in QML via `JSON.parse`.
        #[qproperty(QString, report_json, cxx_name = "reportJson")]
        /// Non-empty when the last scan attempt failed (pkexec missing,
        /// scanner binary missing, polkit prompt denied, etc.).
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// True while a scan is in flight (polkit prompt + the scan itself
        /// can both take real wall-clock time).
        #[qproperty(bool, busy)]
        type ScannerController = super::ScannerControllerRust;

        /// Run one privileged deep scan off the UI thread. Wired to the
        /// Security Scan page's "Run Deep Scan" button.
        #[qinvokable]
        #[cxx_name = "runScan"]
        fn run_scan(self: Pin<&mut ScannerController>);
    }

    impl cxx_qt::Threading for ScannerController {}
}

/// Rust-side state for [`qobject::ScannerController`].
#[derive(Default)]
pub struct ScannerControllerRust {
    report_json: QString,
    error_text: QString,
    busy: bool,
}

impl qobject::ScannerController {
    fn run_scan(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = crate::scanner::run_deep_scan();
            let _ = qt_thread.queue(move |mut qobject| {
                match result {
                    Ok(json) => {
                        qobject.as_mut().set_report_json(QString::from(&json));
                        qobject.as_mut().set_error_text(QString::default());
                    }
                    Err(e) => {
                        qobject.as_mut().set_error_text(QString::from(&e));
                    }
                }
                qobject.as_mut().set_busy(false);
            });
        });
    }
}
