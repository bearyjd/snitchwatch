//! An edit of a rule under its own name (P2.1 re-review).
//!
//! opensnitchd's `replaceUserRule` deletes an `always` rule's file *before*
//! it compiles a temporary replacement (`deleteOldRuleFromDisk`), and a
//! compile error then answers ERROR with the old rule still applying, but
//! no longer saved: it would be gone after the next restart. So when such
//! an edit is refused, the old rule is sent again as it was, which writes
//! its file back, and the result says so.

use super::steps::{change, step, Step};
use crate::replier::display_reason;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Notification, Rule};
use std::time::Duration;

const RESTORED: &str = "The firewall had already removed this rule's saved file, so Snitchwatch \
     saved the rule again as it was.";
const NOT_RESTORED: &str = "The firewall had already removed this rule's saved file, and \
     Snitchwatch couldn't save it again: the rule still applies, but only until the firewall \
     restarts. Edit it again, or delete it and create it anew.";

/// Whether the daemon removes `old`'s file before compiling `new`.
fn loses_file(old: &Rule, new: &Rule) -> bool {
    old.duration == "always" && new.duration != "always"
}

pub(super) async fn run(
    commands: &DaemonCommands,
    timeout: Duration,
    edit: Notification,
    old: &Rule,
) -> RuleCommandOutcome {
    let lost = loses_file(old, &edit.rules[0]);
    match step(commands, edit, timeout).await {
        Step::Ok => RuleCommandOutcome::Ok,
        Step::Rejected(text) if lost => {
            let restored = Rule {
                created: 0,
                ..old.clone()
            };
            let note = match step(commands, change(restored), timeout).await {
                Step::Ok => RESTORED,
                _ => NOT_RESTORED,
            };
            RuleCommandOutcome::Rejected {
                reason: format!("{} {note}", display_reason(&text)),
            }
        }
        Step::Rejected(text) => super::wait_outcome(Err(CommandError::Rejected(text))),
        Step::Unanswered => RuleCommandOutcome::Timeout,
        Step::NotSent(outcome) => outcome,
    }
}
