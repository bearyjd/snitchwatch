//! "Make a rule…" for a row whose prompt was put off (prompt-slot plan Part
//! C, item 9), or that the firewall's default action decided (E3): the rule
//! the decision sheet's choices describe, sent as `AddRule` (the daemon's
//! `CHANGE_RULE`), until the rule editor exists.
//!
//! The rule comes from the bridge crate's own `verdict_to_rule`, so it is
//! bound to the program and refused for an unidentifiable one (#44) exactly
//! like the rule an answered prompt would have made. Only a remembered
//! duration makes a rule; a once-only answer can't be added afterwards.
//!
//! Two fixes on what the row carries (PR #98 security review):
//! - **No hostname.** The bridge shows the IP in `dst_host` when the daemon
//!   sent no host (`translator::connection::connection_to_row`), and a
//!   `dest.host == <ip>` rule never matches: the daemon's `DstHost` is
//!   empty. Such a row is built with no host, so the rule matches `dest.ip`.
//!   On port 53 `DstHost` can be a DNS question that equals the IP
//!   (`rules/simulator/prefill.rs`); `dest.ip` still matches there.
//! - **A name of its own.** The daemon's `CHANGE_RULE` replaces a rule of
//!   the same name, and `rule_name_for` gives the same name to the prompt's
//!   own rule, e.g. "Decide later"'s 5-minute block, whose expiry timer
//!   would then delete the new rule. A made rule's name ends in
//!   `-made-<unix ms>`.
//!
//! **Only the bridge's answer says it worked** (PR #108 security review,
//! M1). The `AddRule` carries a request id, and the sheet says "The rule was
//! created." only when the `RuleCommandResult` for it is Ok. A refusal (the
//! TCP transport, a name clash, the rule policy, a daemon error) shows its
//! reason, and silence past [`NO_ANSWER_AFTER`] says the outcome is unknown.
//! [`MakeRuleWait`] holds that wait; `crate::make_rule_controller` binds it
//! to QML.

use snitchwatch_bridge::cache::connections::Verdict;
use snitchwatch_bridge::rule_wire::rule_to_wire;
use snitchwatch_bridge::translator::verdict::{rule_name_for, verdict_to_rule};
use snitchwatch_bridge::ws_messages::{
    valid_request_id, ClientMessage, ConnectionRow, RuleCommandOutcome, ServerMessage,
};
use snitchwatch_proto::protocol::Connection;
use std::time::{Duration, Instant};

use crate::connections::level_rules::{resolve, Level, NodeRef};
use crate::pending_decision::{parse_duration, parse_scope, VerdictChoice};
pub(crate) use crate::rules::editor_view::{Finished, NO_ANSWER_AFTER};
use crate::rules::editor_view::{Pending, Wording};
use crate::settings::Settings;

/// While the bridge hasn't answered yet.
pub(crate) const SENDING: &str = "Sending the rule to the firewall…";
/// Only for an Ok result.
pub(crate) const CREATED: &str = "The rule was created.";
/// Nothing was sent: no rule may be made, or the bridge isn't reachable.
pub(crate) const NOT_SENT: &str = "The rule couldn't be sent.";
/// No result in time: the rule may or may not exist.
pub(crate) const NO_ANSWER: &str =
    "No answer from the firewall in time. The rule may have been created; check the Rules page.";
const NOT_CREATED: &str = "The rule wasn't created: ";
const REFUSED: &str = "The rule wasn't sent: ";
const NO_DAEMON: &str = "The firewall service isn't connected, so the rule wasn't sent.";

/// Whether "Make a rule…" is offered for `row`: its prompt was put off, or
/// the firewall's default action decided it (E3, plan
/// `2026-10-08-default-applied-events.md`): a rule-matched row has its rule.
pub(crate) fn offers_make_rule(row: &ConnectionRow) -> bool {
    row.deferred || row.decided_by_default
}

/// The `AddRule` for `row` ([`offers_make_rule`]) and the sheet's `choice`,
/// `scope` and `duration` tokens, made at `now_ms` (Unix ms), asking for a
/// `RuleCommandResult` under `request_id`; or `None` when no rule may be
/// made: the row isn't offered one, the duration is once-only, the choice
/// is unknown, the bridge can't name the program, or the request id isn't
/// valid (without one, no outcome could be shown).
pub(crate) fn add_rule_message(
    row: &ConnectionRow,
    choice: &str,
    scope: &str,
    duration: &str,
    now_ms: i64,
    request_id: &str,
) -> Option<ClientMessage> {
    if !offers_make_rule(row) || !valid_request_id(request_id) {
        return None;
    }
    let verdict = match VerdictChoice::from_token(choice)? {
        VerdictChoice::Allow => Verdict::Allow,
        VerdictChoice::Deny => Verdict::Deny,
    };
    let duration = parse_duration(duration);
    if !duration.remembers() {
        return None;
    }
    let ip_stands_in = !row.dst_ip.is_empty() && row.dst_host == row.dst_ip;
    let conn = Connection {
        protocol: row.protocol.clone(),
        dst_host: if ip_stands_in {
            String::new()
        } else {
            row.dst_host.clone()
        },
        dst_ip: row.dst_ip.clone(),
        dst_port: u32::from(row.dst_port),
        process_path: row.process_path.clone().unwrap_or_default(),
        ..Default::default()
    };
    let made = verdict_to_rule(verdict, duration, parse_scope(scope), &conn, now_ms / 1000).ok()?;
    let rule = snitchwatch_proto::protocol::Rule {
        name: format!("{}-made-{now_ms}", made.name),
        ..made
    };
    Some(ClientMessage::AddRule {
        rule: rule_to_wire(&rule),
        request_id: Some(request_id.to_owned()),
        reply: None,
    })
}

/// Why a level rule (a tree node's rule button) wasn't made. Fixed
/// sentences ([`Self::reason`]), safe to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelRuleRefusal {
    /// The request id isn't valid: no outcome could be shown.
    BadRequestId,
    /// The choice isn't allow or deny.
    UnknownChoice,
    /// A program-wide rule needs the user's confirmation.
    NeedsConfirmation,
    /// No absolute program path to bind the rule to (issue #44).
    UnknownProgram,
    /// The Settings duration is "once", which can't be added afterwards.
    NotRemembered,
    /// A public suffix (`co.uk`): no rule can cover the whole domain.
    TooBroad,
    /// The row's level isn't program, domain or host.
    UnknownLevel,
}

impl LevelRuleRefusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::BadRequestId => "The rule couldn't be sent.",
            Self::UnknownChoice => "Choose Allow or Deny.",
            Self::NeedsConfirmation => "A rule for every connection of a program needs confirming.",
            Self::UnknownProgram => {
                "Snitchwatch can't tell which program file this is, so a rule can't be \
                 tied to it."
            }
            Self::NotRemembered => {
                "Your default duration is \"once\", which a rule can't have. Choose another \
                 default duration in Settings."
            }
            Self::UnknownLevel => "A rule can't be made for this row.",
            Self::TooBroad => {
                "This name covers a whole group of unrelated sites, so no one rule can cover \
                 it. Make rules for its hosts."
            }
        }
    }
}

/// A level rule's `AddRule`, and the operator it carries (what a pending
/// command is matched by).
#[derive(Debug)]
pub(crate) struct LevelRuleRequest {
    pub(crate) message: ClientMessage,
    pub(crate) operator: serde_json::Value,
}

/// The `AddRule` for a tree node's rule button, made at `now_ms` (Unix ms),
/// asking for a `RuleCommandResult` under `request_id`. Unlike
/// [`add_rule_message`] it needs no row the prompt was put off for
/// ([`offers_make_rule`] stays for the sheet). The rule is bound to the
/// program; its duration is the Settings default (`settings.duration_token()`).
///
/// A program-wide rule (`AnyHost`) is made only when `confirmed` (the confirm
/// dialog names scope and duration). Nothing is made when the program can't
/// be bound, the default duration is once-only, or the domain is a public
/// suffix; the refusal says which.
pub(crate) fn add_level_rule_message(
    node: NodeRef<'_>,
    choice: &str,
    settings: &Settings,
    confirmed: bool,
    now_ms: i64,
    request_id: &str,
) -> Result<LevelRuleRequest, LevelRuleRefusal> {
    use LevelRuleRefusal as Refusal;
    if !valid_request_id(request_id) {
        return Err(Refusal::BadRequestId);
    }
    let verdict = match VerdictChoice::from_token(choice).ok_or(Refusal::UnknownChoice)? {
        VerdictChoice::Allow => Verdict::Allow,
        VerdictChoice::Deny => Verdict::Deny,
    };
    if node.level == Level::Program && !confirmed {
        return Err(Refusal::NeedsConfirmation);
    }
    let duration = parse_duration(settings.duration_token());
    if !duration.remembers() {
        return Err(Refusal::NotRemembered);
    }
    let target = resolve(node);
    if target.too_broad {
        return Err(Refusal::TooBroad);
    }
    let operator = target.operator.clone().ok_or(Refusal::UnknownProgram)?;
    let made = verdict_to_rule(verdict, duration, target.scope, &target.conn, now_ms / 1000)
        .map_err(|_| Refusal::UnknownProgram)?;
    // The bridge names a rule after the host, which for a domain is made up;
    // name it for the node, and keep the `-made-` suffix (see the module doc).
    let name = rule_name_for(verdict, &target.label, 0, &target.conn.process_path);
    let rule = snitchwatch_proto::protocol::Rule {
        name: format!("{name}-made-{now_ms}"),
        ..made
    };
    Ok(LevelRuleRequest {
        message: ClientMessage::AddRule {
            rule: rule_to_wire(&rule),
            request_id: Some(request_id.to_owned()),
            reply: None,
        },
        operator,
    })
}

/// What the sheet says when the rule wasn't sent, or `None` when `send`
/// queued it: [`NOT_SENT`] when no rule may be made (`message` is `None`),
/// otherwise the rule editor's text for why it wasn't queued.
pub(crate) fn send_problem(
    message: Option<ClientMessage>,
    send: impl FnOnce(ClientMessage) -> Result<(), crate::bridge_runtime::SendClientMessageError>,
) -> Option<&'static str> {
    let Some(message) = message else {
        return Some(NOT_SENT);
    };
    send(message).err().map(crate::rule_commands::not_sent_text)
}

/// "Make a rule…"'s wording of a result: [`CREATED`] only for Ok.
const WORDING: Wording = Wording {
    saved: CREATED,
    not_saved: NOT_CREATED,
    not_sent: REFUSED,
    no_daemon: NO_DAEMON,
    unknown: NO_ANSWER,
    note_after_saved: true,
};

/// What the sheet says for the bridge's `outcome` (what [`MakeRuleWait`]
/// finishes with). Plain text; `saved` means the rule was created.
#[cfg(test)]
pub(crate) fn outcome_status(outcome: &RuleCommandOutcome) -> Finished {
    crate::rules::editor_view::finished_with(outcome, &WORDING)
}

/// How a request ended, for the off-screen notice (`MakeRuleOutcomes.qml`
/// reads these numbers): only Ok is created, and when the outcome isn't
/// known (no result in time, the session gone, the bridge's own timeout or
/// an unsure result) it is unknown, never "not created" (PR #111).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    NotCreated = 0,
    Created = 1,
    Unknown = 2,
}

/// The [`Ending`] of the bridge's `outcome`.
pub fn ending_of(outcome: &RuleCommandOutcome) -> Ending {
    match outcome {
        RuleCommandOutcome::Ok | RuleCommandOutcome::OkWithNote { .. } => Ending::Created,
        RuleCommandOutcome::Timeout | RuleCommandOutcome::Unsure { .. } => Ending::Unknown,
        RuleCommandOutcome::Rejected { .. }
        | RuleCommandOutcome::Refused { .. }
        | RuleCommandOutcome::NoDaemon => Ending::NotCreated,
    }
}

/// The row a request is about, and the row's bridge session (from its
/// local id).
#[derive(Debug)]
struct RowTag {
    row_id: String,
    session: Option<u64>,
}

/// The one "Make a rule…" request waiting for the bridge's result.
#[derive(Debug, Default)]
pub(crate) struct MakeRuleWait(Pending<RowTag>);

impl MakeRuleWait {
    pub(crate) fn begin(&mut self, request_id: String, row_id: String, now: Instant) {
        let session =
            crate::rule_commands::split_session_row_id(&row_id).map(|(session, _)| session);
        self.0
            .sent_with(request_id, RowTag { row_id, session }, now);
    }

    #[cfg(test)]
    pub(crate) fn is_waiting(&self) -> bool {
        self.0.is_waiting()
    }

    /// The send failed: nothing to wait for.
    pub(crate) fn abandon(&mut self) {
        self.0.abandon();
    }

    /// The row and how it ended, when `message` is the awaited result.
    pub(crate) fn on_message(
        &mut self,
        message: &ServerMessage,
    ) -> Option<(String, Finished, Ending)> {
        let ServerMessage::RuleCommandResult { outcome, .. } = message else {
            return None;
        };
        let ending = ending_of(outcome);
        self.0
            .result_with(message, &WORDING)
            .map(|(tag, done)| (tag.row_id, done, ending))
    }

    /// Gives up after `after` of silence ([`NO_ANSWER_AFTER`] outside the
    /// probes), or at once when the row's bridge session isn't
    /// `is_current` any more: a result goes only to the connection that
    /// asked, so after a reconnect none can come.
    pub(crate) fn poll(
        &mut self,
        now: Instant,
        after: Duration,
        is_current: impl Fn(u64) -> bool,
    ) -> Option<(String, Finished, Ending)> {
        let gone = |tag: &RowTag| tag.session.is_some_and(|session| !is_current(session));
        self.0
            .expired_with(now, after, gone, &WORDING)
            .map(|(tag, done)| (tag.row_id, done, Ending::Unknown))
    }
}

#[cfg(test)]
mod level_tests;

#[cfg(test)]
mod tests;
