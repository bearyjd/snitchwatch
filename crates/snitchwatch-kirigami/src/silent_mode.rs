//! Silent mode (LS redesign phase 1): answering a new prompt without asking.
//!
//! When the user chose "Allow and record" or "Deny and record",
//! `BridgeFeed`'s feed task answers each NEW pending row of this GUI's own
//! bridge session at arrival with `SetVerdict(chosen, this_host, once)`, and
//! switching to one of those modes answers the rows already waiting
//! ([`answer_pending_now`], same dedup). "Ask me" answers nothing: the alert
//! window (`AlertWindow.qml`) is independent of the main window and asks
//! whether that window is shown or hidden to the tray. The answer is
//! once-only on purpose: it keeps the connection working without storing a
//! rule nobody reviewed, and the row is marked "needs a rule" (the
//! `needsRule` role) so the user can make one later.
//!
//! **Why Deny is once-only too.** An inline Deny sends `until_restart` so the
//! SYN the kernel resends is not asked about again. A silent Deny does not:
//! that is a remembered rule the user never chose, and the row would not
//! "need a rule" any more. The cost is that a retransmit arrives as a new row
//! and is denied (and listed) again.
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
use std::sync::{Mutex, OnceLock};

use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

use crate::connections::outcome::is_pending;
use crate::pending_decision::build_verdict_message;
use crate::settings::SilentMode;

/// The verdict token silent mode answers with, if it answers at all. "Ask me"
/// does not, whether or not the main window is shown.
pub(crate) fn choice_for(mode: SilentMode) -> Option<&'static str> {
    match mode {
        SilentMode::Allow => Some("allow"),
        SilentMode::Deny => Some("deny"),
        SilentMode::Off => None,
    }
}

/// What the settings last said. Until `AlertHost` reports it nothing is
/// answered silently.
static STATE: Mutex<SilentMode> = Mutex::new(SilentMode::Off);

/// Called from QML (`BridgeFeed.setSilentState`) when the mode changes.
pub(crate) fn set_state(mode: SilentMode) {
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = mode;
}

/// The verdict token silent mode answers with right now, if any.
pub(crate) fn current_choice() -> Option<&'static str> {
    choice_for(*STATE.lock().unwrap_or_else(|p| p.into_inner()))
}

/// The rows answered so far, shared by the feed task and the sweep that runs
/// when the mode changes, so a row is never answered twice.
pub(crate) fn shared_answered() -> &'static Mutex<Answered> {
    static ANSWERED: OnceLock<Mutex<Answered>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
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
        self.enter(session);
        rows.iter()
            .filter(|row| is_pending(row) && self.wire_ids.insert(row.id.clone()))
            .filter_map(|row| answer_for(session, &row.id, choice))
            .collect()
    }

    /// The silent answers for rows already known to be pending in `session`
    /// (`wire_ids`), not answered before.
    pub(crate) fn plan_waiting(
        &mut self,
        session: u64,
        wire_ids: &[String],
        choice: &str,
    ) -> Vec<(String, ClientMessage)> {
        self.enter(session);
        wire_ids
            .iter()
            .filter(|id| self.wire_ids.insert((*id).clone()))
            .filter_map(|id| answer_for(session, id, choice))
            .collect()
    }

    /// Take back a row whose answer could not be sent (`qualified` as in a
    /// plan), so it can be planned again.
    pub(crate) fn forget(&mut self, qualified: &str) {
        let Some((session, wire_id)) = qualified.split_once(':') else {
            return;
        };
        if session.parse::<u64>().ok() == self.session {
            self.wire_ids.remove(wire_id);
        }
    }

    fn enter(&mut self, session: u64) {
        if self.session != Some(session) {
            self.session = Some(session);
            self.wire_ids.clear();
        }
    }
}

fn answer_for(session: u64, wire_id: &str, choice: &str) -> Option<(String, ClientMessage)> {
    let qualified = format!("{session}:{wire_id}");
    let msg = build_verdict_message(&qualified, choice, "this_host", "this_time")?;
    Some((qualified, msg))
}

/// The feed task's hook: answer the new pending rows in `message` if silent
/// mode says to. Runs on the bridge runtime; the send is a non-blocking
/// `try_send`.
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
    let plan = answered.plan(session, message, choice);
    send_plan(handles, answered, plan);
}

/// Silent mode was just switched to Allow or Deny: answer the rows of the
/// current session that are already waiting, like [`answer_new_rows`] would
/// have at arrival. Does nothing in "Ask me".
pub(crate) fn answer_pending_now(
    handles: &crate::bridge_runtime::BridgeHandles,
    answered: &mut Answered,
) {
    let Some(choice) = current_choice() else {
        return;
    };
    let Some((session, wire_ids)) = handles.pending_wire_ids() else {
        return;
    };
    let plan = answered.plan_waiting(session, &wire_ids, choice);
    send_plan(handles, answered, plan);
}

/// Send each answer once-only; a row is marked "needs a rule" only once its
/// answer is queued, so a row that was never answered does not claim it was.
/// A row whose send failed is forgotten, so it is not left unanswered and
/// unasked: the next snapshot or sweep tries it again, and a switch back to
/// "Ask me" lets the alert show it.
fn send_plan(
    handles: &crate::bridge_runtime::BridgeHandles,
    answered: &mut Answered,
    plan: Vec<(String, ClientMessage)>,
) {
    for (qualified, msg) in plan {
        match crate::bridge_feed::dispatch_to(handles, msg, false) {
            Ok(()) => mark_needs_rule(&qualified),
            Err(error) => {
                answered.forget(&qualified);
                tracing::warn!(%error, row = %qualified, "silent answer not sent");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::ws_messages::{
        ConnectionRow, VerdictAction, VerdictDuration, VerdictScope,
    };

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
    fn the_chosen_verdict_applies_and_ask_me_answers_nothing() {
        assert_eq!(choice_for(SilentMode::Allow), Some("allow"));
        assert_eq!(choice_for(SilentMode::Deny), Some("deny"));
        // The alert window asks; nothing is auto-answered, whatever the
        // main window is doing.
        assert_eq!(choice_for(SilentMode::Off), None);
    }

    #[test]
    fn waiting_rows_are_answered_once_and_share_the_dedup_with_new_rows() {
        let mut answered = Answered::default();
        let ids = vec!["w-1".to_string(), "w-2".to_string()];
        let plan = answered.plan_waiting(4, &ids, "deny");
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].0, "4:w-1");
        assert!(answered.plan_waiting(4, &ids, "deny").is_empty());
        // A snapshot repeating a swept row does not answer it again.
        assert!(answered
            .plan(4, &insert(vec![row("w-1", None)]), "deny")
            .is_empty());
        // A new session asks afresh.
        assert_eq!(answered.plan_waiting(5, &ids, "deny").len(), 2);
    }

    #[test]
    fn a_row_whose_answer_was_not_sent_is_planned_again() {
        let mut answered = Answered::default();
        let snapshot = insert(vec![row("ask-1", None)]);
        let plan = answered.plan(2, &snapshot, "deny");
        assert_eq!(plan.len(), 1);
        answered.forget(&plan[0].0);
        assert_eq!(answered.plan(2, &snapshot, "deny").len(), 1);
        // Another session's id, or a malformed one, forgets nothing.
        answered.forget("9:ask-1");
        answered.forget("nonsense");
        assert!(answered.plan(2, &snapshot, "deny").is_empty());
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
