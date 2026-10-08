//! One daemon command of a multi-step rule command (a rename, an edit and
//! its restore), and what came of it.

use super::send_error_outcome;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Action, Notification, Rule};
use std::time::Duration;

/// What one step's command came to.
pub(super) enum Step {
    Ok,
    Rejected(String),
    /// Sent, no answer: it may or may not have happened.
    Unanswered,
    /// Never sent.
    NotSent(RuleCommandOutcome),
}

pub(super) async fn step(
    commands: &DaemonCommands,
    notification: Notification,
    timeout: Duration,
) -> Step {
    match commands.send(notification) {
        Err(error) => Step::NotSent(send_error_outcome(error)),
        Ok(pending) => match pending.wait(timeout).await {
            Ok(()) => Step::Ok,
            Err(CommandError::Rejected(text)) => Step::Rejected(text),
            Err(CommandError::Timeout | CommandError::StreamClosed) => Step::Unanswered,
        },
    }
}

pub(super) fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(super) fn change(rule: Rule) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule],
        ..Default::default()
    }
}
