//! What the Connections page's inline buttons (a row's Allow/Deny, a process
//! header's "Allow all"/"Deny all") send for one row. Plan:
//! `docs/superpowers/plans/2026-10-08-inline-deny-until-restart.md`.
//!
//! **Why Deny is remembered.** opensnitchd never stores a `once` rule, so a
//! once-only deny drops only the packet that asked. The kernel resends the SYN
//! about a second later, and that retransmit is asked about again or gets the
//! daemon's default action. So an inline Deny sends `until_restart` (daemon
//! `"until restart"`), which the bridge binds to the program and this host.
//!
//! **When it can't be.** It falls back to once-only, and the page says why:
//! * [`InlineDeny::ProgramUnknown`]: the row has no program path the bridge
//!   can bind a rule to (issue #44; the bridge would answer once anyway);
//! * [`InlineDeny::BridgeTooOld`]: the row's bridge session didn't advertise
//!   `appBoundRules` (`snitchwatch_bridge::bridge_capabilities`). Bridges
//!   before #50/#71 build "This host" rules for every app, so a remembered
//!   deny there would block every program from that host until restart.
//!
//! The program comes first: it is a property of the row, true on any bridge.
//!
//! **Allow follows the Settings duration.** The daemon stores only `always`
//! rules, so a once-only Allow means the same prompt next time. An inline
//! Allow therefore sends the user's default duration (`settings`, "Forever"
//! unless changed), but only where a Deny could be remembered too: a bindable
//! program on a bridge with app-bound rules. Anywhere else it is once-only and
//! the page says why, as for Deny (`InlineDeny`'s other two cases).

use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;

use crate::connections::row_store::RowStore;
use crate::pending_decision::VerdictChoice;

/// What an inline Deny does for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineDeny {
    /// Remembered until the daemon restarts, bound to the program and host.
    UntilRestart,
    /// Once only: no program file the bridge could bind a rule to.
    ProgramUnknown,
    /// Once only: the row's bridge can't bind a rule to the program.
    BridgeTooOld,
}

impl InlineDeny {
    /// `process_path` is the row's; `app_bound_rules` whether its bridge
    /// session advertised app-bound rules.
    pub(crate) fn decide(process_path: Option<&str>, app_bound_rules: bool) -> Self {
        if !process_path.is_some_and(is_bindable_process_path) {
            Self::ProgramUnknown
        } else if !app_bound_rules {
            Self::BridgeTooOld
        } else {
            Self::UntilRestart
        }
    }

    /// [`Self::decide`] for the row with `id`. An unknown id is
    /// [`Self::ProgramUnknown`]: the bridge rejects a verdict for a row that
    /// is gone anyway (#49).
    pub(crate) fn for_row(store: &RowStore, id: &str, app_bound_rules: bool) -> Self {
        let process_path = store
            .row_by_id(id)
            .and_then(|row| row.process_path.as_deref());
        Self::decide(process_path, app_bound_rules)
    }

    /// The stable token QML reads (`ConnectionsModel.inlineDenyFor`).
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::UntilRestart => "until_restart",
            Self::ProgramUnknown => "program_unknown",
            Self::BridgeTooOld => "bridge_too_old",
        }
    }

    /// The duration token (`pending_decision::parse_duration`) a Deny sends.
    pub(crate) fn duration_token(self) -> &'static str {
        match self {
            Self::UntilRestart => "until_restart",
            Self::ProgramUnknown | Self::BridgeTooOld => "this_time",
        }
    }
}

/// The duration token an inline `choice` sends for the row with `id`
/// (`ConnectionsModel.inlineDurationFor`). `allow_duration` is the Settings
/// default (`settings::normalize_duration` makes it a known token).
pub(crate) fn duration_token_for_row(
    store: &RowStore,
    id: &str,
    choice: VerdictChoice,
    app_bound_rules: bool,
    allow_duration: &str,
) -> &'static str {
    let deny = InlineDeny::for_row(store, id, app_bound_rules);
    match choice {
        VerdictChoice::Deny => deny.duration_token(),
        VerdictChoice::Allow => allow_token(deny, allow_duration),
    }
}

/// What an Allow sends: the Settings duration where the row's program and
/// bridge can bind a rule (`deny` is [`InlineDeny::UntilRestart`]), else
/// `this_time`.
pub(crate) fn allow_token(deny: InlineDeny, allow_duration: &str) -> &'static str {
    match deny {
        InlineDeny::UntilRestart => crate::settings::normalize_duration(allow_duration),
        InlineDeny::ProgramUnknown | InlineDeny::BridgeTooOld => "this_time",
    }
}

/// The label of an Allow button that sends [`allow_token`]: plain "Allow"
/// when it is once-only (by choice or after a downgrade), else with the
/// duration it will remember, e.g. "Allow (Forever)".
pub(crate) fn allow_label(deny: InlineDeny, allow_duration: &str) -> String {
    match allow_token(deny, allow_duration) {
        "this_time" => "Allow".to_string(),
        token => format!("Allow ({})", crate::settings::duration_label(token)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_decision::build_verdict_message;
    use snitchwatch_bridge::ws_messages::{ClientMessage, ConnectionRow, VerdictDuration};

    /// Program paths an inline Deny can't be remembered for: none, the
    /// daemon's placeholder, a bare comm name, a relative path.
    const UNBINDABLE_PATHS: [Option<&str>; 5] = [
        None,
        Some(""),
        Some("Kernel connection"),
        Some("curl"),
        Some("bin/curl"),
    ];

    fn store_with(rows: &[(&str, Option<&str>)]) -> RowStore {
        let mut store = RowStore::new();
        store.insert_rows(
            rows.iter()
                .map(|(id, path)| ConnectionRow {
                    id: id.to_string(),
                    process: "p".to_string(),
                    process_path: path.map(str::to_string),
                    dst_host: "github.com".to_string(),
                    dst_ip: "140.82.112.3".to_string(),
                    dst_port: 443,
                    protocol: "tcp".to_string(),
                    direction: "outgoing".to_string(),
                    action: None,
                    bytes_sent: 0,
                    bytes_received: 0,
                    started_at_ms: 0,
                    matched_rule: None,
                    auto_answer: None,
                    answer_deadline_ms: None,
                    deferred: false,
                    decided_by_default: false,
                })
                .collect(),
        );
        store
    }

    #[test]
    fn the_allow_label_names_the_duration_it_will_remember() {
        use InlineDeny::*;
        assert_eq!(allow_label(UntilRestart, "forever"), "Allow (Forever)");
        assert_eq!(
            allow_label(UntilRestart, "until_restart"),
            "Allow (Until firewall restarts)"
        );
        assert_eq!(
            allow_label(UntilRestart, "for_5_minutes"),
            "Allow (5 minutes)"
        );
        assert_eq!(allow_label(UntilRestart, "this_time"), "Allow");
        // Downgraded: honest about it being once-only.
        assert_eq!(allow_label(ProgramUnknown, "forever"), "Allow");
        assert_eq!(allow_label(BridgeTooOld, "forever"), "Allow");
    }

    #[test]
    fn an_inline_deny_is_remembered_only_for_a_bindable_program_on_a_capable_bridge() {
        use InlineDeny::*;
        assert_eq!(
            InlineDeny::decide(Some("/usr/bin/curl"), true),
            UntilRestart
        );
        assert_eq!(
            InlineDeny::decide(Some("/usr/bin/curl"), false),
            BridgeTooOld
        );
        for path in UNBINDABLE_PATHS {
            for app_bound_rules in [true, false] {
                // The program comes first: true whatever the bridge.
                assert_eq!(
                    InlineDeny::decide(path, app_bound_rules),
                    ProgramUnknown,
                    "{path:?} {app_bound_rules}"
                );
            }
        }
    }

    #[test]
    fn only_a_remembered_deny_sends_until_restart_and_allow_stays_once_when_the_default_is_once() {
        let store = store_with(&[
            ("abs", Some("/usr/bin/curl")),
            ("kernel", Some("Kernel connection")),
        ]);
        let sent = |id, choice, app_bound_rules| {
            duration_token_for_row(&store, id, choice, app_bound_rules, "this_time")
        };
        assert_eq!(sent("abs", VerdictChoice::Deny, true), "until_restart");
        assert_eq!(sent("abs", VerdictChoice::Deny, false), "this_time");
        assert_eq!(sent("kernel", VerdictChoice::Deny, true), "this_time");
        assert_eq!(sent("gone", VerdictChoice::Deny, true), "this_time");
        for id in ["abs", "kernel", "gone"] {
            for app_bound_rules in [true, false] {
                assert_eq!(sent(id, VerdictChoice::Allow, app_bound_rules), "this_time");
            }
        }
    }

    #[test]
    fn an_inline_allow_remembers_for_the_settings_duration_where_a_rule_can_be_bound() {
        let store = store_with(&[
            ("abs", Some("/usr/bin/curl")),
            ("kernel", Some("Kernel connection")),
        ]);
        let allow = |id, app_bound_rules, default| {
            duration_token_for_row(&store, id, VerdictChoice::Allow, app_bound_rules, default)
        };
        for default in ["this_time", "for_5_minutes", "until_restart", "forever"] {
            assert_eq!(allow("abs", true, default), default);
            // No rule can be bound: once-only whatever the default.
            assert_eq!(allow("abs", false, default), "this_time");
            assert_eq!(allow("kernel", true, default), "this_time");
            assert_eq!(allow("gone", true, default), "this_time");
        }
        // The pre-rename token and a garbled one both stay on the safe side
        // of the vocabulary: a known token.
        assert_eq!(allow("abs", true, "until_quit"), "until_restart");
        assert_eq!(allow("abs", true, "nonsense"), "forever");
    }

    #[test]
    fn row_lookup_and_tokens() {
        let store = store_with(&[
            ("abs", Some("/usr/bin/curl")),
            ("kernel", Some("Kernel connection")),
        ]);
        assert_eq!(
            InlineDeny::for_row(&store, "abs", true).token(),
            "until_restart"
        );
        assert_eq!(
            InlineDeny::for_row(&store, "abs", false).token(),
            "bridge_too_old"
        );
        assert_eq!(
            InlineDeny::for_row(&store, "kernel", true).token(),
            "program_unknown"
        );
        // The bridge rejects a verdict for a row that is gone anyway (#49).
        assert_eq!(
            InlineDeny::for_row(&store, "gone", true).token(),
            "program_unknown"
        );
    }

    /// `parse_duration` turns any unknown token into `Once`, so check the
    /// parsed duration and the exact wire JSON, not only the QML token. The
    /// JSON is what `tests/bridge_protocol_test.rs`'s inline-Deny round trip
    /// sends.
    #[test]
    fn inline_tokens_reach_the_wire_as_until_restart_or_once() {
        let store = store_with(&[
            ("abs", Some("/usr/bin/curl")),
            ("kernel", Some("Kernel connection")),
        ]);
        let deny = |id, app_bound_rules| {
            let token =
                duration_token_for_row(&store, id, VerdictChoice::Deny, app_bound_rules, "forever");
            build_verdict_message("r1", "deny", "this_host", token).unwrap()
        };
        let duration = |msg| match msg {
            ClientMessage::SetVerdict { duration, .. } => duration,
            other => panic!("expected SetVerdict, got {other:?}"),
        };
        assert_eq!(
            duration(deny("abs", true)),
            Some(VerdictDuration::UntilRestart)
        );
        assert_eq!(
            serde_json::to_value(deny("abs", true)).unwrap(),
            serde_json::json!({
                "action": "setVerdict",
                "rowId": "r1",
                "verdict": "deny",
                "scope": "this_host",
                "duration": "until_restart",
            })
        );
        assert_eq!(duration(deny("abs", false)), Some(VerdictDuration::Once));
        assert_eq!(duration(deny("kernel", true)), Some(VerdictDuration::Once));
    }
}
