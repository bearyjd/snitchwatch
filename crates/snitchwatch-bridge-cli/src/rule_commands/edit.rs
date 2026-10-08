//! An edit of a rule under its own name (P2.1 re-review).
//!
//! opensnitchd's `replaceUserRule` deletes an `always` rule's file *before*
//! it compiles a temporary replacement (`deleteOldRuleFromDisk`), and a
//! compile error then answers ERROR with the old rule still applying, but
//! no longer saved. With live reload (on by default) the removed file also
//! makes the daemon drop the rule from memory. So when such an edit is
//! refused, the old rule is sent again as it was, which brings it and its
//! file back; if that fails, or the edit goes unanswered, the rule may be
//! gone: it leaves the bridge's list and the result says so.

use super::steps::{change, step, Step};
use crate::replier::display_reason;
use snitchwatch_bridge::cache::rules::SharedRulesCache;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Notification, Rule};
use std::time::Duration;

const RESTORED: &str = "The firewall had already removed this rule's saved file, so Snitchwatch \
     saved the rule again as it was. For a moment it may not have applied.";
const GONE: &str = "The firewall had already removed this rule's saved file, and Snitchwatch \
     couldn't save it again: the rule may have stopped applying. Check the Rules page after the \
     firewall's next update, and create it again if it's gone.";
const UNANSWERED_LOST: &str = "No answer from the firewall in time. If it made the change, it \
     removed this rule's saved file first, so the rule may have stopped applying. Check the Rules \
     page after the firewall's next update.";

/// Whether the daemon removes `old`'s file before compiling `new`.
fn loses_file(old: &Rule, new: &Rule) -> bool {
    old.duration == "always" && new.duration != "always"
}

/// With live reload (the daemon's default) a removed file also drops the
/// rule from memory, so the bridge's list mustn't claim it any more.
fn forget(rules: &SharedRulesCache, name: &str) {
    rules.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
}

pub(super) async fn run(
    commands: &DaemonCommands,
    rules: &SharedRulesCache,
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
                _ => {
                    forget(rules, &old.name);
                    GONE
                }
            };
            RuleCommandOutcome::Rejected {
                reason: format!("{} {note}", display_reason(&text)),
            }
        }
        Step::Rejected(text) => super::wait_outcome(Err(CommandError::Rejected(text))),
        Step::Unanswered if lost => {
            forget(rules, &old.name);
            RuleCommandOutcome::Unsure {
                reason: UNANSWERED_LOST.to_string(),
            }
        }
        Step::Unanswered => RuleCommandOutcome::Timeout,
        Step::NotSent(outcome) => outcome,
    }
}
