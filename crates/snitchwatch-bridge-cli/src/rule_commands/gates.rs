//! What a rule command may do, decided before anything is sent (P2.1). See
//! the parent module's doc for the rules.

use snitchwatch_bridge::cache::rules::{RulesCache, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport};
use snitchwatch_bridge::rule_io::only_enabled_differs;
use snitchwatch_bridge::rule_policy::{
    check_wire_rule, read_only_reason, validate_user_rule, PolicyProfile, RuleProblem,
};
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::UpstreamEffect;
use snitchwatch_proto::protocol::{Notification, Rule};
use std::collections::HashSet;
use std::sync::{Mutex as StdMutex, MutexGuard};

pub(crate) const NAME_TAKEN: &str = "A rule with that name already exists. Choose another name.";
pub(crate) const TCP_REFUSED: &str = "Adding, editing and renaming rules need the system \
     Snitchwatch service. In this per-user setup another program could pose as the firewall \
     service. You can still turn rules on or off and delete them.";
pub(crate) const BUSY: &str = "This rule is being renamed. Try again in a moment.";
const NOT_LOADED: &str = "Rules haven't loaded from the firewall yet.";
const NOT_FOUND: &str = "The firewall has no rule by that name any more.";
const HIDDEN: &str = "This rule is too large for Snitchwatch to show, so it can't be changed here.";

pub(super) enum Command {
    Add {
        rule: serde_json::Value,
    },
    Update {
        rule_id: String,
        rule: serde_json::Value,
    },
    Delete {
        rule_id: String,
    },
}

pub(super) enum Plan {
    Send(Notification),
    Rename {
        change: Notification,
        old: String,
        new: String,
    },
}

type Refusal = Vec<RuleProblem>;

fn refusal(path: &str, reason: &str) -> Refusal {
    vec![RuleProblem {
        path: path.into(),
        reason: reason.into(),
    }]
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The command to send, or why not. Holds the cache lock only while
/// deciding; nothing is sent here.
pub(super) fn plan(
    command: &Command,
    commands: &DaemonCommands,
    rules: &SharedRulesCache,
    busy: &StdMutex<HashSet<String>>,
) -> Result<Plan, Refusal> {
    let on_tcp = commands.transport() == DaemonTransport::Tcp;
    let is_busy = |name: &str| lock(busy).contains(name);
    match command {
        Command::Delete { rule_id } => {
            if is_busy(rule_id) {
                return Err(refusal("name", BUSY));
            }
            let effect = UpstreamEffect::DeleteRule {
                rule_id: rule_id.clone(),
            };
            notification(&effect).map(Plan::Send)
        }
        Command::Add { rule } => {
            if on_tcp {
                return Err(refusal("rule", TCP_REFUSED));
            }
            let parsed = check_wire_rule(rule, PolicyProfile::Editor)?;
            let cache = lock(rules);
            name_free(&cache, &parsed.name, &is_busy)?;
            drop(cache);
            let effect = UpstreamEffect::AddRule { rule: rule.clone() };
            let mut sent = notification(&effect)?;
            stamp_created(&mut sent, None);
            Ok(Plan::Send(sent))
        }
        Command::Update { rule_id, rule } => {
            let cache = lock(rules);
            let old = changeable(&cache, rule_id, &is_busy)?;
            let effect = UpstreamEffect::UpdateRule {
                rule_id: rule_id.clone(),
                rule: rule.clone(),
            };
            let mut sent = notification(&effect)?;
            let new = &sent.rules[0];
            if new.name == *rule_id && only_enabled_differs(&old, new) {
                return Ok(Plan::Send(sent));
            }
            if on_tcp {
                return Err(refusal("rule", TCP_REFUSED));
            }
            validate_user_rule(new, PolicyProfile::Editor)?;
            let new_name = new.name.clone();
            if new_name != *rule_id {
                name_free(&cache, &new_name, &is_busy)?;
            }
            // A renamed rule is a new rule to the daemon.
            stamp_created(&mut sent, (new_name == *rule_id).then_some(&old));
            Ok(if new_name == *rule_id {
                Plan::Send(sent)
            } else {
                Plan::Rename {
                    change: sent,
                    old: rule_id.clone(),
                    new: new_name,
                }
            })
        }
    }
}

/// The daemon notification for an effect; `rule_from_wire`, the reserved
/// names and `validate_operator` all run here.
fn notification(effect: &UpstreamEffect) -> Result<Notification, Refusal> {
    match notification_for_effect(effect, 0) {
        Ok(Some(notification)) => Ok(notification),
        Ok(None) => Err(refusal("rule", "not a rule command")),
        Err(reason) => Err(refusal("rule", &reason)),
    }
}

/// A new name must be unused: not cached, not hidden, not being renamed.
fn name_free(
    cache: &RulesCache,
    name: &str,
    is_busy: &impl Fn(&str) -> bool,
) -> Result<(), Refusal> {
    let rules = cache.rules().ok_or_else(|| refusal("rule", NOT_LOADED))?;
    if rules.contains_key(name) || cache.left_out().contains_key(name) {
        return Err(refusal("name", NAME_TAKEN));
    }
    if is_busy(name) {
        return Err(refusal("name", BUSY));
    }
    Ok(())
}

/// The cached rule `rule_id` names, if Snitchwatch may change it.
fn changeable(
    cache: &RulesCache,
    rule_id: &str,
    is_busy: &impl Fn(&str) -> bool,
) -> Result<Rule, Refusal> {
    let rules = cache.rules().ok_or_else(|| refusal("rule", NOT_LOADED))?;
    if cache.left_out().contains_key(rule_id) {
        return Err(refusal("rule", HIDDEN));
    }
    let old = rules
        .get(rule_id)
        .ok_or_else(|| refusal("rule", NOT_FOUND))?;
    if let Some(reason) = read_only_reason(old) {
        return Err(refusal("rule", reason));
    }
    if is_busy(rule_id) {
        return Err(refusal("name", BUSY));
    }
    Ok(old.clone())
}

/// The daemon starts a timed rule's clock when it stores it, and a changed
/// duration restarts it (`loader.go` `scheduleTemporaryRule`; an unchanged
/// one keeps its old timer). Stamp `created` so the cache expires it then.
fn stamp_created(sent: &mut Notification, old: Option<&Rule>) {
    let rule = &mut sent.rules[0];
    let timed = !matches!(rule.duration.as_str(), "always" | "until restart" | "once");
    let restarted = old.is_none_or(|old| old.duration != rule.duration);
    if timed && restarted {
        rule.created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }
}
