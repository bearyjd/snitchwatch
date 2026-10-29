//! Live daemon/kernel readiness status, driven by
//! `ServerMessage::DiagnosticsReport`. Mirrors `tray_controller.rs`'s
//! bridge-feed wiring pattern (confirmed scalar-`qproperty`-based QObject
//! shape — not `rules_model.rs`'s `QAbstractListModel` shape), since this is
//! a fixed four-check summary, not a growing collection.

use core::pin::Pin;
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use snitchwatch_bridge::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck, ServerMessage};

/// True if any check in the report is `Failed`.
fn has_problem(checks: &[DiagnosticCheck]) -> bool {
    checks
        .iter()
        .any(|c| matches!(c.status, CheckStatus::Failed { .. }))
}

/// True only when the report has a `DaemonReachable` check and it is `Ok`.
/// The bridge's watchdog pushes a fresh report the moment opensnitchd drops,
/// so this goes false while the bridge link itself stays up.
fn daemon_reachable(checks: &[DiagnosticCheck]) -> bool {
    checks
        .iter()
        .any(|c| c.kind == CheckKind::DaemonReachable && c.status == CheckStatus::Ok)
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

/// Fixed sentences returned by [`status_summary`]. The summary goes into
/// `main.qml`'s `Kirigami.InlineMessage`, which renders its text as markup
/// (issue #51), so it must never carry a check's detail: that text comes from
/// the bridge's daemon and kernel probes and belongs in the PlainText
/// troubleshooting label.
const SUMMARY_HEALTHY: &str = "Daemon reachable; firewall checks passed";
const SUMMARY_PENDING: &str = "Waiting for complete daemon health information";
const SUMMARY_PROBLEM: &str = "Daemon or firewall issue reported — see Daemon Health for details";

fn status_summary(checks: &[DiagnosticCheck]) -> String {
    if has_problem(checks) {
        SUMMARY_PROBLEM.to_string()
    } else if [
        CheckKind::DaemonReachable,
        CheckKind::FirewallRunning,
        CheckKind::NftablesSupport,
    ]
    .iter()
    .all(|kind| {
        let mut matching = checks.iter().filter(|c| c.kind == *kind);
        matches!(matching.next(), Some(check) if check.status == CheckStatus::Ok)
            && matching.next().is_none()
    }) {
        SUMMARY_HEALTHY.to_string()
    } else {
        SUMMARY_PENDING.to_string()
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
        /// True while the latest report says opensnitchd is reachable; false
        /// when it says otherwise or no report has arrived yet.
        #[qproperty(bool, daemon_reachable, cxx_name = "daemonReachable")]
        /// One-line human-readable summary of overall health.
        #[qproperty(QString, status_summary, cxx_name = "statusSummary")]
        /// Joined troubleshooting detail for every failed check, empty when
        /// healthy.
        #[qproperty(QString, troubleshooting_text, cxx_name = "troubleshootingText")]
        /// Q04: always `tray_health::MONITOR_NOT_REPORTED`. opensnitchd
        /// never reports the running process monitor, so no report sets it.
        #[qproperty(QString, monitor_status, cxx_name = "monitorStatus")]
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
    daemon_reachable: bool,
    status_summary: QString,
    troubleshooting_text: QString,
    monitor_status: QString,
}

impl Default for DaemonHealthModelRust {
    fn default() -> Self {
        Self {
            has_problem: false,
            daemon_reachable: false,
            status_summary: QString::from(SUMMARY_PENDING),
            troubleshooting_text: QString::default(),
            monitor_status: QString::from(crate::tray_health::MONITOR_NOT_REPORTED),
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
            .set_daemon_reachable(daemon_reachable(&checks));
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
        // The current report arrives in the snapshot `spawn_feed` requests.
        // No automatic `RecheckDiagnostics`: the bridge clears its stored
        // daemon alerts before answering one, so opening the GUI would erase
        // a reported firewall or monitor failure. Only "Recheck" sends it.
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

    fn reachable_check(status: CheckStatus) -> DiagnosticCheck {
        DiagnosticCheck {
            kind: CheckKind::DaemonReachable,
            status,
        }
    }

    #[test]
    fn daemon_reachable_needs_an_ok_daemon_check() {
        assert!(daemon_reachable(&[reachable_check(CheckStatus::Ok)]));
        assert!(!daemon_reachable(&[]), "no report yet is not reachable");
        assert!(!daemon_reachable(&[DiagnosticCheck {
            kind: CheckKind::FirewallRunning,
            status: CheckStatus::Ok,
        }]));
        assert!(!daemon_reachable(&[reachable_check(CheckStatus::Failed {
            detail: "down".to_string()
        })]));
    }

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
        assert_eq!(status_summary(&checks), SUMMARY_HEALTHY);
    }

    #[test]
    fn unreported_monitor_method_allows_healthy_proc_or_ebpf() {
        let checks = [
            (CheckKind::DaemonReachable, CheckStatus::Ok),
            (CheckKind::FirewallRunning, CheckStatus::Ok),
            (CheckKind::NftablesSupport, CheckStatus::Ok),
            (CheckKind::EbpfSupport, CheckStatus::Unknown),
        ]
        .map(|(kind, status)| DiagnosticCheck { kind, status });
        assert!(!has_problem(&checks));
        assert_eq!(status_summary(&checks), SUMMARY_HEALTHY);
        assert!(troubleshooting_text(&checks).is_empty());
    }

    #[test]
    fn incomplete_required_health_stays_pending() {
        assert_eq!(status_summary(&[]), SUMMARY_PENDING);
        for missing in [
            CheckKind::DaemonReachable,
            CheckKind::FirewallRunning,
            CheckKind::NftablesSupport,
        ] {
            let checks = [
                CheckKind::DaemonReachable,
                CheckKind::FirewallRunning,
                CheckKind::NftablesSupport,
            ]
            .map(|kind| DiagnosticCheck {
                kind,
                status: if kind == missing {
                    CheckStatus::Unknown
                } else {
                    CheckStatus::Ok
                },
            });
            assert_eq!(status_summary(&checks), SUMMARY_PENDING);
            assert!(!has_problem(&checks));
        }
    }

    #[test]
    fn duplicate_required_checks_never_report_healthy() {
        for duplicate_kind in [
            CheckKind::DaemonReachable,
            CheckKind::FirewallRunning,
            CheckKind::NftablesSupport,
        ] {
            for duplicate_status in [
                CheckStatus::Unknown,
                CheckStatus::Ok,
                CheckStatus::Failed {
                    detail: "duplicate failed".to_string(),
                },
            ] {
                for insert_first in [false, true] {
                    let expected = if matches!(duplicate_status, CheckStatus::Failed { .. }) {
                        SUMMARY_PROBLEM
                    } else {
                        SUMMARY_PENDING
                    };
                    let mut checks = [
                        CheckKind::DaemonReachable,
                        CheckKind::FirewallRunning,
                        CheckKind::NftablesSupport,
                    ]
                    .map(|kind| DiagnosticCheck {
                        kind,
                        status: CheckStatus::Ok,
                    })
                    .to_vec();
                    let duplicate = DiagnosticCheck {
                        kind: duplicate_kind,
                        status: duplicate_status.clone(),
                    };
                    if insert_first {
                        checks.insert(0, duplicate);
                    } else {
                        checks.push(duplicate);
                    }
                    assert_eq!(status_summary(&checks), expected);
                }
            }
        }
    }

    #[test]
    fn derive_status_summary_flags_failed_check_with_its_detail() {
        let checks = vec![DiagnosticCheck {
            kind: CheckKind::EbpfSupport,
            status: CheckStatus::Failed {
                detail: "process monitor failed".to_string(),
            },
        }];
        assert!(has_problem(&checks));
        assert!(troubleshooting_text(&checks).contains("process monitor failed"));
    }

    #[test]
    fn the_summary_uses_fixed_sentences_whatever_the_details_say() {
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
                summary == SUMMARY_HEALTHY
                    || summary == SUMMARY_PROBLEM
                    || summary == SUMMARY_PENDING,
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
