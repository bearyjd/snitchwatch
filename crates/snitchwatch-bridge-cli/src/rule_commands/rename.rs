//! Rename (P2.1, E1): `CHANGE_RULE` the new name, then, only after its OK,
//! `DELETE_RULE` the old name. It isn't atomic: between the steps both
//! rules exist, and the old one still decides first if it sorts first.
//!
//! What a failure leaves:
//! - the new rule refused: only the old one, unchanged;
//! - the old rule's delete refused: the new one is deleted again, so only
//!   the old one is left; if that fails too, both exist, and it says so;
//! - the old rule's delete unanswered: it may have worked, and deleting the
//!   new one then could leave neither (a deny lost), so nothing is undone
//!   and it says both may exist.
//!
//! "Neither" can't result; "both" only after a second failure or an
//! unanswered delete, and the result always says which.

use super::{send_error_outcome, wait_outcome};
use crate::replier::display_reason;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::time::Duration;

const NEW_UNCONFIRMED: &str = "Snitchwatch couldn't confirm the renamed rule was saved, so the \
     old rule was left as it is. Check the Rules page: both may exist.";
const UNDONE: &str = "The firewall service couldn't remove the old rule, so the renamed copy \
     was removed again. Nothing changed.";
const BOTH_EXIST: &str = "The renamed rule was saved, but the old one couldn't be removed, and \
     neither could the new one: both exist. Delete one on the Rules page.";
const BOTH_MAY_EXIST: &str = "The renamed rule was saved, but Snitchwatch couldn't confirm the \
     old one was removed: both may exist. Check the Rules page.";

/// What one step's command came to.
enum Step {
    Ok,
    Rejected(String),
    /// Sent, no answer: it may or may not have happened.
    Unanswered,
    /// Never sent.
    NotSent(RuleCommandOutcome),
}

async fn step(commands: &DaemonCommands, notification: Notification, timeout: Duration) -> Step {
    match commands.send(notification) {
        Err(error) => Step::NotSent(send_error_outcome(error)),
        Ok(pending) => match pending.wait(timeout).await {
            Ok(()) => Step::Ok,
            Err(CommandError::Rejected(text)) => Step::Rejected(text),
            Err(CommandError::Timeout | CommandError::StreamClosed) => Step::Unanswered,
        },
    }
}

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn unsure(reason: &str) -> RuleCommandOutcome {
    RuleCommandOutcome::Unsure {
        reason: reason.to_string(),
    }
}

pub(super) async fn run(
    commands: &DaemonCommands,
    timeout: Duration,
    change: Notification,
    old: &str,
    new: &str,
) -> RuleCommandOutcome {
    match step(commands, change, timeout).await {
        Step::Ok => {}
        Step::Rejected(text) => return wait_outcome(Err(CommandError::Rejected(text))),
        Step::Unanswered => return unsure(NEW_UNCONFIRMED),
        Step::NotSent(outcome) => return outcome,
    }
    match step(commands, delete(old), timeout).await {
        Step::Ok => RuleCommandOutcome::Ok,
        Step::Rejected(text) => match step(commands, delete(new), timeout).await {
            Step::Ok => RuleCommandOutcome::Rejected {
                reason: format!("{UNDONE} ({})", display_reason(&text)),
            },
            _ => unsure(BOTH_EXIST),
        },
        Step::NotSent(_) => unsure(BOTH_EXIST),
        Step::Unanswered => unsure(BOTH_MAY_EXIST),
    }
}
