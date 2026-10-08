//! Live daemon/kernel readiness status, driven by
//! `ServerMessage::DiagnosticsReport`. Mirrors `tray_controller.rs`'s
//! bridge-feed wiring pattern (confirmed scalar-`qproperty`-based QObject
//! shape — not `rules_model.rs`'s `QAbstractListModel` shape), since this is
//! a fixed four-check summary, not a growing collection.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use snitchwatch_bridge::ws_messages::{CheckStatus, DiagnosticCheck, ServerMessage};

/// True if any check in the report is `Failed`.
fn has_problem(checks: &[DiagnosticCheck]) -> bool {
    checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Failed { .. }))
}

/// Joins every failed check's troubleshooting detail, one per line. Empty
/// string when nothing has failed.
fn troubleshooting_text(checks: &[DiagnosticCheck]) -> String {
    checks
        .iter()
        .filter_map(|c| match &c.status {
            CheckStatus::Failed { detail } => Some(detail.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The only two sentences [`status_summary`] returns. The summary goes into
/// `main.qml`'s `Kirigami.InlineMessage`, which renders its text as markup
/// (issue #51), so it must never carry a check's detail: that text comes from
/// the bridge's daemon and kernel probes and belongs in the PlainText
/// troubleshooting label.
const SUMMARY_HEALTHY: &str = "Everything looks healthy";
const SUMMARY_PROBLEM: &str =
    "Connection or kernel problem detected — see Daemon Health for details";

fn status_summary(checks: &[DiagnosticCheck]) -> String {
    if has_problem(checks) {
        SUMMARY_PROBLEM.to_string()
    } else {
        SUMMARY_HEALTHY.to_string()
    }
}

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        /// Daemon/kernel readiness summary, bound by `DaemonHealthPage.qml`
        /// and the `main.qml` banner.
        #[qobject]
        #[qml_element]
        /// True if any check in the latest report failed.
        #[qproperty(bool, has_problem, cxx_name = "hasProblem")]
        /// One-line human-readable summary of overall health.
        #[qproperty(QString, status_summary, cxx_name = "statusSummary")]
        /// Joined troubleshooting detail for every failed check, empty when
        /// healthy.
        #[qproperty(QString, troubleshooting_text, cxx_name = "troubleshootingText")]
        type DaemonHealthModel = super::DaemonHealthModelRust;

        /// Apply a JSON-encoded `ServerMessage` — only `DiagnosticsReport`
        /// updates the properties; anything else is ignored.
        #[qinvokable]
        #[cxx_name = "applyServerMessageJson"]
        fn apply_server_message_json(self: Pin<&mut DaemonHealthModel>, json: &QString);

        /// Start the live feed: subscribe to the bridge's outbound stream and
        /// update properties on every `DiagnosticsReport`. No-op when the
        /// bridge isn't running. Called from QML's `Component.onCompleted`.
        #[qinvokable]
        #[cxx_name = "startBridgeFeed"]
        fn start_bridge_feed(self: Pin<&mut DaemonHealthModel>);

        /// Ask the bridge to re-run its diagnostics checks now.
        #[qinvokable]
        fn recheck(self: Pin<&mut DaemonHealthModel>);
    }

    impl cxx_qt::Threading for DaemonHealthModel {}
}

/// Rust-side state for [`qobject::DaemonHealthModel`].
pub struct DaemonHealthModelRust {
    has_problem: bool,
    status_summary: QString,
    troubleshooting_text: QString,
}

impl Default for DaemonHealthModelRust {
    fn default() -> Self {
        Self {
            has_problem: false,
            status_summary: QString::from(SUMMARY_HEALTHY),
            troubleshooting_text: QString::default(),
        }
    }
}

impl qobject::DaemonHealthModel {
    fn apply_server_message_json(self: Pin<&mut Self>, json: &QString) {
        let text = json.to_string();
        let Ok(ServerMessage::DiagnosticsReport { checks }) =
            serde_json::from_str::<ServerMessage>(&text)
        else {
            return;
        };
        let mut this = self;
        this.as_mut().set_has_problem(has_problem(&checks));
        this.as_mut()
            .set_status_summary(QString::from(&status_summary(&checks)));
        this.as_mut()
            .set_troubleshooting_text(QString::from(&troubleshooting_text(&checks)));
    }

    fn start_bridge_feed(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("DaemonHealthModel: bridge not running; live feed disabled");
            return;
        };
        let qt_thread = self.qt_thread();
        let session_handles = handles.clone();
        crate::bridge_dispatch::spawn_feed(
            &handles,
            "DaemonHealthModel",
            crate::bridge_dispatch::interests_diagnostics,
            move |connection_id, _msg, json| {
                let session_handles = session_handles.clone();
                let _ = qt_thread.queue(move |qobject| {
                    if !session_handles.is_current_session(connection_id) {
                        return;
                    }
                    qobject.apply_server_message_json(&QString::from(&json));
                });
            },
        );

        // The bridge's one-time startup `DiagnosticsReport` fires from
        // `ensure_started()` in `main()`, before QML (and thus this
        // subscription) exists — tokio broadcast channels don't replay to
        // late subscribers, so that report is otherwise lost forever. Pull
        // the current state immediately so a kernel-prerequisite problem
        // still surfaces without the user having to click "Recheck".
        if let Err(error) =
            handles.try_send(snitchwatch_bridge::ws_messages::ClientMessage::RecheckDiagnostics)
        {
            tracing::warn!(error = %error, "DaemonHealthModel: initial diagnostics recheck ignored");
        }
    }

    fn recheck(self: Pin<&mut Self>) {
        let Some(handles) = crate::bridge_runtime::handles() else {
            tracing::warn!("DaemonHealthModel: bridge not running; recheck ignored");
            return;
        };
        if let Err(error) =
            handles.try_send(snitchwatch_bridge::ws_messages::ClientMessage::RecheckDiagnostics)
        {
            tracing::warn!(error = %error, "DaemonHealthModel: diagnostics recheck ignored");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::CheckKind;

    #[test]
    fn derive_status_summary_ok_when_all_checks_pass() {
        let checks = vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::EbpfSupport,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::NftablesSupport,
                status: CheckStatus::Ok,
            },
        ];
        assert!(!has_problem(&checks));
        assert_eq!(troubleshooting_text(&checks), "");
    }

    #[test]
    fn derive_status_summary_flags_failed_check_with_its_detail() {
        let checks = vec![DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: CheckStatus::Failed {
                detail: "no BTF".to_string(),
            },
        }];
        assert!(has_problem(&checks));
        assert!(troubleshooting_text(&checks).contains("no BTF"));
    }

    #[test]
    fn the_summary_is_one_of_two_fixed_sentences_whatever_the_details_say() {
        let markup = "<b>bold</b> <img src='https://example.invalid/x.png'> &amp;";
        let failed = |kind| DiagnosticCheck {
            kind,
            status: CheckStatus::Failed {
                detail: markup.to_string(),
            },
        };
        let reports = [
            vec![],
            vec![DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Unknown,
            }],
            vec![failed(CheckKind::EbpfSupport)],
            vec![
                failed(CheckKind::DaemonReachable),
                failed(CheckKind::FirewallRunning),
                failed(CheckKind::EbpfSupport),
                failed(CheckKind::NftablesSupport),
            ],
        ];
        for checks in reports {
            let summary = status_summary(&checks);
            assert!(
                summary == SUMMARY_HEALTHY || summary == SUMMARY_PROBLEM,
                "{summary}"
            );
            assert!(
                !summary.contains('<') && !summary.contains("example.invalid"),
                "the summary carries check text: {summary}"
            );
        }
        // The detail still reaches the troubleshooting text.
        assert!(troubleshooting_text(&[failed(CheckKind::EbpfSupport)]).contains(markup));
    }
}
