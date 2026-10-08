//! What the Connections page's inline buttons (a row's Allow/Deny, a process
//! header's "Allow all"/"Deny all") send for one row. Plan:
//! `docs/superpowers/plans/2026-10-08-inline-deny-until-restart.md`.
//!
//! **Why Deny is remembered.** opensnitchd never stores a `once` rule, so a
//! once-only deny drops only the packet that asked. The kernel resends the SYN
//! about a second later, and that retransmit is asked about again or gets the
//! daemon's default action. So an inline Deny sends `until_quit` (daemon
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
//! **Allow stays once.** An accepted SYN establishes the flow, so once already
//! does what it says, and remembering an allow is a trust grant an inline
//! click shouldn't make.

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
    fn duration_token(self) -> &'static str {
        match self {
            Self::UntilRestart => "until_quit",
            Self::ProgramUnknown | Self::BridgeTooOld => "this_time",
        }
    }
}

/// The duration token an inline `choice` sends for the row with `id`
/// (`ConnectionsModel.inlineDurationFor`).
pub(crate) fn duration_token_for_row(
    store: &RowStore,
    id: &str,
    choice: VerdictChoice,
    app_bound_rules: bool,
) -> &'static str {
    match choice {
        VerdictChoice::Deny => InlineDeny::for_row(store, id, app_bound_rules).duration_token(),
        VerdictChoice::Allow => "this_time",
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
                })
                .collect(),
        );
        store
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
    fn only_a_remembered_deny_sends_until_quit_and_allow_stays_once() {
        let store = store_with(&[
            ("abs", Some("/usr/bin/curl")),
            ("kernel", Some("Kernel connection")),
        ]);
        let sent = |id, choice, app_bound_rules| {
            duration_token_for_row(&store, id, choice, app_bound_rules)
        };
        assert_eq!(sent("abs", VerdictChoice::Deny, true), "until_quit");
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
            let token = duration_token_for_row(&store, id, VerdictChoice::Deny, app_bound_rules);
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
