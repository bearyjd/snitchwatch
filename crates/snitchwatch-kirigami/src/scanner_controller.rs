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
use cxx_qt::{CxxQtType, Threading};
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
        #[qproperty(QString, error_title, cxx_name = "errorTitle")]
        /// True while a scan is in flight (polkit prompt + the scan itself
        /// can both take real wall-clock time).
        #[qproperty(bool, busy)]
        /// One line stating the outcome of the last scan attempt (clean,
        /// findings or failure); empty before the first attempt.
        #[qproperty(QString, summary_text, cxx_name = "summaryText")]
        /// "clean", "findings" or "error"; empty before the first attempt.
        #[qproperty(QString, summary_kind, cxx_name = "summaryKind")]
        type ScannerController = super::ScannerControllerRust;

        /// Run one privileged deep scan off the UI thread. Wired to the
        /// Security Scan page's "Run Deep Scan" button.
        #[qinvokable]
        #[cxx_name = "runScan"]
        fn run_scan(self: Pin<&mut ScannerController>);

        /// The outcome line of a finished scan that has not been announced to
        /// assistive tech yet, or empty. Returns it and clears it, so each
        /// scan is announced at most once, even if the page that was
        /// listening when it finished is gone (navigated away) by then.
        #[qinvokable]
        #[cxx_name = "takePendingOutcome"]
        fn take_pending_outcome(self: Pin<&mut ScannerController>) -> QString;
    }

    impl cxx_qt::Threading for ScannerController {}
}

/// Rust-side state for [`qobject::ScannerController`].
#[derive(Default)]
pub struct ScannerControllerRust {
    report_json: QString,
    error_text: QString,
    error_title: QString,
    busy: bool,
    summary_text: QString,
    summary_kind: QString,
    pending_outcome: QString,
}

impl qobject::ScannerController {
    fn take_pending_outcome(mut self: Pin<&mut Self>) -> QString {
        std::mem::take(&mut self.as_mut().rust_mut().pending_outcome)
    }

    fn run_scan(mut self: Pin<&mut Self>) {
        if self.busy {
            return;
        }
        tracing::info!("deep scan requested");
        // The previous outcome (error header, summary line) stays up until
        // this scan completes and replaces it; it must not vanish at start.
        // An outcome nobody heard must not be announced over the new scan.
        self.as_mut().rust_mut().pending_outcome = QString::default();
        self.as_mut().set_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = crate::scanner::run_deep_scan();
            match &result {
                Ok(json) => tracing::info!(report_bytes = json.len(), "deep scan replied: ok"),
                Err(e) => tracing::warn!(error_title = e.title, "deep scan failed"),
            }
            let (kind, summary) = crate::scanner::summarize(&result, &crate::scanner::local_hhmm());
            let queued = qt_thread.queue(move |mut qobject| {
                qobject
                    .as_mut()
                    .set_summary_kind(QString::from(kind.as_str()));
                qobject.as_mut().set_summary_text(QString::from(&summary));
                qobject.as_mut().rust_mut().pending_outcome = QString::from(&summary);
                match result {
                    Ok(json) => {
                        qobject.as_mut().set_report_json(QString::from(&json));
                        qobject.as_mut().set_error_text(QString::default());
                        qobject.as_mut().set_error_title(QString::default());
                    }
                    Err(e) => {
                        qobject.as_mut().set_error_title(QString::from(e.title));
                        qobject.as_mut().set_error_text(QString::from(&e.detail));
                    }
                }
                qobject.as_mut().set_busy(false);
                tracing::info!(
                    summary_kind = kind.as_str(),
                    "deep scan outcome applied to the UI"
                );
            });
            if let Err(error) = queued {
                tracing::error!(%error, "deep scan finished but the result could not be delivered to the UI");
            }
        });
    }
}
