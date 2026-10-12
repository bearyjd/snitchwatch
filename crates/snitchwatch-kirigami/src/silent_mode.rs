//! Silent mode (LS redesign phase 1): answering a new prompt without asking.
//!
//! When the user chose "Allow and record" or "Deny and record", or the main
//! window is hidden (closing it only hides it to the tray, so nobody is
//! looking), `BridgeFeed`'s feed task answers each NEW pending row of this
//! GUI's own bridge session at arrival with `SetVerdict(chosen, this_host,
//! once)`. A hidden window with silent mode off allows. The answer is
//! once-only on purpose: it keeps the connection working without storing a
//! rule nobody reviewed, and the row is marked "needs a rule" (the
//! `needsRule` role) so the user can make one later.
//!
//! **Why a GUI-side set and not `ConnectionRow.auto_answer`:** `AutoAnswer` is
//! the bridge's wire enum (`FilterPaused`, `NoAnswer`) and means "the bridge
//! answered"; this GUI answered, and the bridge's semantics stay as they are.
//! The set holds session-qualified row ids (`<connection>:<wire id>`, as
//! `ConnectionsModel` stores them), capped so a long session cannot grow it
//! without bound.
//!
//! Only this GUI's current session is answered
//! (`BridgeHandles::is_current_session`); the bridge ignores a second answer
//! for a row that has stopped waiting, so two GUIs cannot double-apply.
//!
//! Qt-free: the pure pieces are unit-tested below.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use snitchwatch_bridge::ws_messages::{ClientMessage, ConnectionRow, ServerMessage};

use crate::connections::outcome::is_pending;
use crate::pending_decision::build_verdict_message;
use crate::settings::SilentMode;

/// The verdict token silent mode answers with, if it answers at all.
/// A hidden window with silent mode off allows: a prompt nobody can see would
/// otherwise wait out the bridge's 30 s and fall to the firewall's default.
pub(crate) fn choice_for(mode: SilentMode, window_visible: bool) -> Option<&'static str> {
    match (mode, window_visible) {
        (SilentMode::Allow, _) => Some("allow"),
        (SilentMode::Deny, _) => Some("deny"),
        (SilentMode::Off, false) => Some("allow"),
        (SilentMode::Off, true) => None,
    }
}

/// What the window and settings last said. The window starts "visible": until
/// `main.qml` reports otherwise nothing is answered silently.
static STATE: Mutex<(SilentMode, bool)> = Mutex::new((SilentMode::Off, true));

/// Called from QML (`BridgeFeed.setSilentState`) when either input changes.
pub(crate) fn set_state(mode: SilentMode, window_visible: bool) {
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = (mode, window_visible);
}

/// The verdict token silent mode answers with right now, if any.
pub(crate) fn current_choice() -> Option<&'static str> {
    let (mode, visible) = *STATE.lock().unwrap_or_else(|p| p.into_inner());
    choice_for(mode, visible)
}

const NEEDS_RULE_CAP: usize = 4096;

#[derive(Default)]
struct Registry {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

static NEEDS_RULE: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut guard = NEEDS_RULE.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(Registry::default))
}

/// Remember that the GUI answered `qualified_id` once-only, silently.
pub(crate) fn mark_needs_rule(qualified_id: &str) {
    with_registry(|registry| {
        if !registry.ids.insert(qualified_id.to_string()) {
            return;
        }
        registry.order.push_back(qualified_id.to_string());
        while registry.order.len() > NEEDS_RULE_CAP {
            if let Some(oldest) = registry.order.pop_front() {
                registry.ids.remove(&oldest);
            }
        }
    });
}

/// Whether `qualified_id` was answered silently and still has no rule.
pub(crate) fn needs_rule(qualified_id: &str) -> bool {
    with_registry(|registry| registry.ids.contains(qualified_id))
}

/// The rows already answered in one bridge session, so a resync snapshot that
/// repeats them is not answered twice. Forgets everything when the session
/// changes: a new bridge has new prompts.
#[derive(Default)]
pub(crate) struct Answered {
    session: Option<u64>,
    wire_ids: HashSet<String>,
}

impl Answered {
    /// The silent answers `message` calls for in `session`, as
    /// `(qualified row id, SetVerdict)`. Only rows still pending that this
    /// instance has not answered before.
    pub(crate) fn plan(
        &mut self,
        session: u64,
        message: &ServerMessage,
        choice: &str,
    ) -> Vec<(String, ClientMessage)> {
        let (ServerMessage::InsertConnectionRows { rows }
        | ServerMessage::UpdateConnectionRows { rows }) = message
        else {
            return Vec::new();
        };
        if self.session != Some(session) {
            self.session = Some(session);
            self.wire_ids.clear();
        }
        rows.iter()
            .filter(|row| is_pending(row) && self.wire_ids.insert(row.id.clone()))
            .filter_map(|row| answer_for(session, row, choice))
            .collect()
    }
}

fn answer_for(session: u64, row: &ConnectionRow, choice: &str) -> Option<(String, ClientMessage)> {
    let qualified = format!("{session}:{}", row.id);
    let msg = build_verdict_message(&qualified, choice, "this_host", "this_time")?;
    Some((qualified, msg))
}

/// The feed task's hook: answer the new pending rows in `message` if silent
/// mode (or a hidden window) says to. Runs on the bridge runtime; the send is
/// a non-blocking `try_send`.
pub(crate) fn answer_new_rows(
    handles: &crate::bridge_runtime::BridgeHandles,
    session: u64,
    message: &ServerMessage,
    answered: &mut Answered,
) {
    let Some(choice) = current_choice() else {
        return;
    };
    if !handles.is_current_session(session) {
        return;
    }
    for (qualified, msg) in answered.plan(session, message, choice) {
        mark_needs_rule(&qualified);
        if let Err(error) = crate::bridge_feed::dispatch_to(handles, msg, false) {
            tracing::warn!(%error, row = %qualified, "silent answer not sent");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::{VerdictAction, VerdictDuration, VerdictScope};

    fn row(id: &str, action: Option<&str>) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "curl".into(),
            process_path: Some("/usr/bin/curl".into()),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            protocol: "tcp".into(),
            direction: "outgoing".into(),
            action: action.map(str::to_string),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
            decided_by_default: false,
        }
    }

    fn insert(rows: Vec<ConnectionRow>) -> ServerMessage {
        ServerMessage::InsertConnectionRows { rows }
    }

    #[test]
    fn the_chosen_verdict_applies_and_a_hidden_window_allows() {
        assert_eq!(choice_for(SilentMode::Allow, true), Some("allow"));
        assert_eq!(choice_for(SilentMode::Allow, false), Some("allow"));
        assert_eq!(choice_for(SilentMode::Deny, true), Some("deny"));
        // Hidden does not turn a chosen Deny into an Allow.
        assert_eq!(choice_for(SilentMode::Deny, false), Some("deny"));
        assert_eq!(choice_for(SilentMode::Off, true), None);
        assert_eq!(choice_for(SilentMode::Off, false), Some("allow"));
    }

    #[test]
    fn a_new_pending_row_gets_a_once_only_this_host_verdict_for_its_session() {
        let mut answered = Answered::default();
        let plan = answered.plan(3, &insert(vec![row("ask-1", None)]), "deny");
        assert_eq!(plan.len(), 1);
        let (qualified, msg) = &plan[0];
        assert_eq!(qualified, "3:ask-1");
        assert_eq!(
            msg,
            &ClientMessage::SetVerdict {
                row_id: "3:ask-1".into(),
                verdict: VerdictAction::Deny,
                scope: VerdictScope::ThisHost,
                duration: Some(VerdictDuration::Once),
                remember: None,
            }
        );
    }

    #[test]
    fn decided_deferred_and_default_rows_are_left_alone() {
        let mut answered = Answered::default();
        let deferred = ConnectionRow {
            deferred: true,
            ..row("ask-2", None)
        };
        let by_default = ConnectionRow {
            decided_by_default: true,
            ..row("ask-3", None)
        };
        let plan = answered.plan(
            1,
            &insert(vec![row("ask-1", Some("allow")), deferred, by_default]),
            "allow",
        );
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn a_row_is_answered_once_per_session_even_when_a_snapshot_repeats_it() {
        let mut answered = Answered::default();
        let snapshot = insert(vec![row("ask-1", None)]);
        assert_eq!(answered.plan(1, &snapshot, "allow").len(), 1);
        assert!(answered.plan(1, &snapshot, "allow").is_empty());
        let update = ServerMessage::UpdateConnectionRows {
            rows: vec![row("ask-1", None), row("ask-2", None)],
        };
        assert_eq!(answered.plan(1, &update, "allow").len(), 1);
        // A new bridge session asks about its own prompts afresh.
        assert_eq!(answered.plan(2, &snapshot, "allow").len(), 1);
    }

    #[test]
    fn other_messages_answer_nothing() {
        let mut answered = Answered::default();
        assert!(answered
            .plan(1, &ServerMessage::ClearConnectionRows, "allow")
            .is_empty());
    }

    #[test]
    fn needs_rule_is_remembered_and_bounded() {
        mark_needs_rule("9:ask-x");
        assert!(needs_rule("9:ask-x"));
        assert!(!needs_rule("9:ask-y"));
        for n in 0..NEEDS_RULE_CAP + 10 {
            mark_needs_rule(&format!("8:{n}"));
        }
        assert!(needs_rule(&format!("8:{}", NEEDS_RULE_CAP + 9)));
        assert!(!needs_rule("8:0"), "the oldest marks are forgotten");
    }
}
