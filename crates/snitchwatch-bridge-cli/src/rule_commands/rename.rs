//! Rename (P2.1, E1): `CHANGE_RULE` the new name, then, only after its OK,
//! `DELETE_RULE` the old name. It isn't atomic: between the steps both
//! rules exist.
//!
//! opensnitchd's `Delete` drops the rule from memory *before* removing its
//! file, and removing the file is the only thing that can fail
//! (`loader.go`). So an ERROR on the old rule's delete means it has already
//! stopped applying: the rename stands, nothing is undone (undoing would
//! leave neither rule), the old rule leaves the bridge's list, and the
//! result says its file may bring it back when the firewall restarts.
//!
//! What a failure leaves:
//! - the new rule refused or never sent: only the old one, unchanged;
//! - the old rule's delete unanswered (it may have worked) or never sent:
//!   both may exist, and the result says which one decides meanwhile.
//!
//! "Neither" can't result.

use super::steps::{delete, step, Step};
use crate::replier::display_reason;
use snitchwatch_bridge::cache::rules::SharedRulesCache;
use snitchwatch_bridge::daemon_commands::{CommandError, DaemonCommands};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::{Notification, Rule};
use std::time::Duration;

const NEW_UNCONFIRMED: &str = "Snitchwatch couldn't confirm the renamed rule was saved, so the \
     old rule was left as it is. Check the Rules page: both may exist.";
const OLD_FILE_LEFT: &str = "Renamed. The firewall stopped using the old rule, but couldn't \
     remove its saved file, so the old rule may come back when the firewall restarts. If it \
     does, delete it on the Rules page.";
const BOTH_EXIST: &str = "The renamed rule was saved, but the old one couldn't be removed: both \
     exist. Delete one on the Rules page.";
const BOTH_MAY_EXIST: &str = "The renamed rule was saved, but Snitchwatch couldn't confirm the \
     old one was removed: both may exist. Check the Rules page.";
pub(super) const OLD_DECIDES: &str = "Until then, where both match, the old rule decides.";
pub(super) const NEW_DECIDES: &str = "Until then, where both match, the renamed rule decides.";
pub(super) const BOTH_OFF: &str = "Both are turned off, so neither decides.";
pub(super) const BOTH_ALLOW: &str =
    "Both allow, so the connections both match are allowed either way.";

fn unsure(reason: &str, old: &Rule, new: &Rule) -> RuleCommandOutcome {
    RuleCommandOutcome::Unsure {
        reason: format!("{reason} {}", deciding(old, new)),
    }
}

/// Which of two rules matching the same connection decides, as
/// opensnitchd's `FindFirstMatch` does: rules in name order, the first
/// matching deny, reject or decide-first rule wins; otherwise the last
/// matching allow. A rule that is off decides nothing.
pub(super) fn deciding(old: &Rule, new: &Rule) -> &'static str {
    let blocks = |r: &Rule| r.precedence || matches!(r.action.as_str(), "deny" | "reject");
    match (old.enabled, new.enabled) {
        (true, false) => return OLD_DECIDES,
        (false, true) => return NEW_DECIDES,
        (false, false) => return BOTH_OFF,
        (true, true) => {}
    }
    match (blocks(old), blocks(new)) {
        (true, true) if old.name < new.name => OLD_DECIDES,
        (true, true) => NEW_DECIDES,
        (true, false) => OLD_DECIDES,
        (false, true) => NEW_DECIDES,
        (false, false) => BOTH_ALLOW,
    }
}

pub(super) async fn run(
    commands: &DaemonCommands,
    rules: &SharedRulesCache,
    timeout: Duration,
    change: Notification,
    old: &Rule,
) -> RuleCommandOutcome {
    let new = change.rules[0].clone();
    match step(commands, change, timeout).await {
        Step::Ok => {}
        Step::Rejected(text) => {
            return super::wait_outcome(Err(CommandError::Rejected(text)));
        }
        Step::Unanswered => {
            return RuleCommandOutcome::Unsure {
                reason: NEW_UNCONFIRMED.to_string(),
            }
        }
        Step::NotSent(outcome) => return outcome,
    }
    match step(commands, delete(&old.name), timeout).await {
        Step::Ok => RuleCommandOutcome::Ok,
        Step::Rejected(text) => {
            // The daemon dropped the old rule before failing on its file.
            rules
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&old.name);
            RuleCommandOutcome::OkWithNote {
                note: format!("{OLD_FILE_LEFT} ({})", display_reason(&text)),
            }
        }
        Step::NotSent(_) => unsure(BOTH_EXIST, old, &new),
        Step::Unanswered => unsure(BOTH_MAY_EXIST, old, &new),
    }
}
