//! Q04: what the tray may say about the firewall and the process monitor
//! (`docs/superpowers/plans/2026-10-27-q04-honest-tray-health.md`).
//!
//! opensnitchd v1.8.0 never reports which process monitor is running, so the
//! monitor line is fixed text. The firewall line comes from the bridge's
//! `FirewallRunning` check, a value stored when the daemon subscribed and
//! re-sent unchanged in every later report: it is labelled as of the daemon's
//! last connection, with the time the GUI noticed it, never the time a report
//! arrived.

use snitchwatch_bridge::diagnostics::FIREWALL_NOT_RUNNING_TROUBLESHOOTING;
use snitchwatch_bridge::ws_messages::{CheckKind, CheckStatus, DiagnosticCheck};

/// The tray's and Daemon Health page's monitor line, whatever any report,
/// About info or daemon config says.
pub const MONITOR_NOT_REPORTED: &str = "Process monitor: not reported by opensnitchd";

/// What the latest report says about the daemon's firewall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallReport {
    NotReported,
    Running,
    /// The subscribe-time `is_firewall_running: false`, with no daemon alert.
    NotRunning,
    /// A failure carrying a daemon alert, or contradictory duplicate checks.
    Issue,
}

/// The `FirewallRunning` check in `checks`.
pub fn firewall_report(checks: &[DiagnosticCheck]) -> FirewallReport {
    let mut matching = checks
        .iter()
        .filter(|c| c.kind == CheckKind::FirewallRunning);
    let Some(first) = matching.next() else {
        return FirewallReport::NotReported;
    };
    if matching.next().is_some() {
        let any_failed = checks.iter().any(|c| {
            c.kind == CheckKind::FirewallRunning && matches!(c.status, CheckStatus::Failed { .. })
        });
        return if any_failed {
            FirewallReport::Issue
        } else {
            FirewallReport::NotReported
        };
    }
    match &first.status {
        CheckStatus::Ok => FirewallReport::Running,
        CheckStatus::Unknown => FirewallReport::NotReported,
        CheckStatus::Failed { detail } if detail == FIREWALL_NOT_RUNNING_TROUBLESHOOTING => {
            FirewallReport::NotRunning
        }
        CheckStatus::Failed { .. } => FirewallReport::Issue,
    }
}

/// The firewall report the tray last saw in one bridge session, and the local
/// "HH:MM" it noticed that report's value change. `noticed` is `None` when the
/// value was already there in the first report of the session: the GUI can't
/// tell how old it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirewallSeen {
    session: u64,
    last: Option<FirewallReport>,
    noticed: Option<String>,
}

impl FirewallSeen {
    /// The state after seeing `report` in `session`. `now` is called only when
    /// the value changed from one seen earlier in the same session; a re-sent
    /// identical value keeps its first time.
    pub fn observe(
        &self,
        session: u64,
        report: FirewallReport,
        now: impl FnOnce() -> String,
    ) -> Self {
        let last = if session == self.session {
            self.last
        } else {
            None
        };
        let noticed = match last {
            Some(previous) if previous == report => self.noticed.clone(),
            Some(_) => Some(now()),
            None => None,
        };
        Self {
            session,
            last: Some(report),
            noticed,
        }
    }

    /// The bridge session this state was seen in.
    pub fn session(&self) -> u64 {
        self.session
    }

    /// The tray's firewall sentence, only while the firewall is reported down
    /// or in trouble.
    pub fn line(&self) -> Option<String> {
        let when = match &self.noticed {
            Some(time) => format!("noticed {time}"),
            None => "time not known".to_string(),
        };
        match self.last? {
            FirewallReport::NotRunning => Some(format!(
                "Firewall not running as of opensnitchd's last connection ({when})"
            )),
            FirewallReport::Issue => Some(format!(
                "Firewall issue reported by opensnitchd ({when}); see Daemon Health"
            )),
            FirewallReport::NotReported | FirewallReport::Running => None,
        }
    }
}

/// `base` (the tray-state sentence), then the firewall line if any, then the
/// monitor line, as sentences. Appended after `base` so they never hide an
/// outage, a pause or pending decisions.
pub fn compose_tooltip(base: &str, firewall: Option<&str>) -> String {
    let mut tooltip = base.to_string();
    for part in firewall.into_iter().chain([MONITOR_NOT_REPORTED]) {
        tooltip.push_str(if tooltip.ends_with('.') { " " } else { ". " });
        tooltip.push_str(part);
    }
    tooltip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(kind: CheckKind, status: CheckStatus) -> DiagnosticCheck {
        DiagnosticCheck { kind, status }
    }

    fn firewall(status: CheckStatus) -> Vec<DiagnosticCheck> {
        vec![
            check(CheckKind::DaemonReachable, CheckStatus::Ok),
            check(CheckKind::FirewallRunning, status),
            check(CheckKind::EbpfSupport, CheckStatus::Unknown),
            check(CheckKind::NftablesSupport, CheckStatus::Ok),
        ]
    }

    fn not_running() -> CheckStatus {
        CheckStatus::Failed {
            detail: FIREWALL_NOT_RUNNING_TROUBLESHOOTING.to_string(),
        }
    }

    fn at(time: &'static str) -> impl FnOnce() -> String {
        move || time.to_string()
    }

    fn never() -> impl FnOnce() -> String {
        || panic!("an unchanged or first-seen value must not read the clock")
    }

    #[test]
    fn firewall_report_reads_the_snapshot_and_alerts() {
        assert_eq!(firewall_report(&[]), FirewallReport::NotReported);
        assert_eq!(
            firewall_report(&firewall(CheckStatus::Unknown)),
            FirewallReport::NotReported
        );
        assert_eq!(
            firewall_report(&firewall(CheckStatus::Ok)),
            FirewallReport::Running
        );
        assert_eq!(
            firewall_report(&firewall(not_running())),
            FirewallReport::NotRunning
        );
        let alert = CheckStatus::Failed {
            detail: format!("{FIREWALL_NOT_RUNNING_TROUBLESHOOTING}\n\nerror: nft failed"),
        };
        assert_eq!(firewall_report(&firewall(alert)), FirewallReport::Issue);
    }

    #[test]
    fn duplicate_firewall_checks_never_read_as_running() {
        let mut checks = firewall(CheckStatus::Ok);
        checks.push(check(CheckKind::FirewallRunning, CheckStatus::Ok));
        assert_eq!(firewall_report(&checks), FirewallReport::NotReported);
        checks.push(check(CheckKind::FirewallRunning, not_running()));
        assert_eq!(firewall_report(&checks), FirewallReport::Issue);
    }

    #[test]
    fn a_change_seen_in_the_session_carries_the_time_it_was_noticed() {
        let seen = FirewallSeen::default()
            .observe(1, FirewallReport::NotReported, never())
            .observe(1, FirewallReport::NotRunning, at("14:30"));
        assert_eq!(
            seen.line().as_deref(),
            Some("Firewall not running as of opensnitchd's last connection (noticed 14:30)")
        );
    }

    #[test]
    fn a_stale_snapshot_resent_later_keeps_its_first_time() {
        let seen = FirewallSeen::default()
            .observe(1, FirewallReport::Running, never())
            .observe(1, FirewallReport::NotRunning, at("14:30"))
            .observe(1, FirewallReport::NotRunning, never())
            .observe(1, FirewallReport::NotRunning, never());
        let line = seen.line().unwrap();
        assert!(line.contains("noticed 14:30"), "{line}");
    }

    #[test]
    fn a_failure_already_there_at_first_sight_has_no_time() {
        let seen = FirewallSeen::default().observe(1, FirewallReport::NotRunning, never());
        assert_eq!(
            seen.line().as_deref(),
            Some("Firewall not running as of opensnitchd's last connection (time not known)")
        );
    }

    #[test]
    fn a_new_bridge_session_forgets_the_old_time() {
        let seen = FirewallSeen::default()
            .observe(1, FirewallReport::NotReported, never())
            .observe(1, FirewallReport::NotRunning, at("14:30"))
            .observe(2, FirewallReport::NotRunning, never());
        assert!(seen.line().unwrap().contains("time not known"));
    }

    #[test]
    fn a_reconnect_after_an_outage_stamps_again() {
        let seen = FirewallSeen::default()
            .observe(1, FirewallReport::NotRunning, never())
            .observe(1, FirewallReport::NotReported, at("14:00"))
            .observe(1, FirewallReport::NotRunning, at("14:45"));
        assert!(seen.line().unwrap().contains("noticed 14:45"));
    }

    #[test]
    fn only_a_down_or_troubled_firewall_gets_a_line() {
        for report in [FirewallReport::NotReported, FirewallReport::Running] {
            let seen = FirewallSeen::default().observe(1, report, never());
            assert_eq!(seen.line(), None, "{report:?}");
        }
        assert_eq!(FirewallSeen::default().line(), None);
        let issue = FirewallSeen::default().observe(1, FirewallReport::Issue, never());
        let line = issue.line().unwrap();
        assert!(line.starts_with("Firewall issue reported by opensnitchd"));
        assert!(!line.contains("not running"), "{line}");
    }

    #[test]
    fn the_tooltip_appends_firewall_then_monitor_after_the_state() {
        assert_eq!(
            compose_tooltip("Snitchwatch — prompting on", None),
            "Snitchwatch — prompting on. Process monitor: not reported by opensnitchd"
        );
        assert_eq!(
            compose_tooltip("3 pending decisions", Some("Firewall issue")),
            "3 pending decisions. Firewall issue. Process monitor: not reported by opensnitchd"
        );
        // A base that already ends a sentence isn't given a second stop.
        assert_eq!(
            compose_tooltip("Paused. Waiting.", None),
            "Paused. Waiting. Process monitor: not reported by opensnitchd"
        );
    }

    #[test]
    fn the_monitor_line_never_names_a_mode() {
        let lower = MONITOR_NOT_REPORTED.to_lowercase();
        assert!(lower.contains("not reported by opensnitchd"));
        for word in ["/proc", "ebpf", "audit", "active", "running", "fallback"] {
            assert!(!lower.contains(word), "{word}");
        }
    }

    /// No tray or health source reads the daemon's configured monitor method
    /// (the only monitor value the bridge ever receives) or the About info's
    /// eBPF build commit.
    #[test]
    fn tray_and_health_sources_never_read_configured_monitor_intent() {
        let sources = [
            ("tray.rs", include_str!("tray.rs")),
            ("tray_health.rs", include_str!("tray_health.rs")),
            ("tray_controller.rs", include_str!("tray_controller.rs")),
            (
                "daemon_health_model.rs",
                include_str!("daemon_health_model.rs"),
            ),
            ("TrayMenu.qml", include_str!("../qml/TrayMenu.qml")),
            (
                "DaemonHealthPage.qml",
                include_str!("../qml/DaemonHealthPage.qml"),
            ),
        ];
        let forbidden = [
            "procmonitor",
            "daemon_config",
            "daemonconfig",
            "ebpf_commit",
        ];
        for (name, source) in sources {
            let lower = source.to_lowercase();
            for word in forbidden {
                let hits = lower.matches(word).count();
                // This file names each forbidden word once, in the list above.
                let allowed = usize::from(name == "tray_health.rs");
                assert!(hits <= allowed, "{name} mentions {word}");
            }
        }
    }
}
