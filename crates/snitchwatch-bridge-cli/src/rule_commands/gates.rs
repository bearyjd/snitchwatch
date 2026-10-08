//! What a rule command may do, decided before anything is sent (P2.1). See
//! the parent module's doc for the rules.

use crate::busy::BusyNames;
use snitchwatch_bridge::cache::rules::{RulesCache, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport};
use snitchwatch_bridge::rule_io::only_enabled_differs;
use snitchwatch_bridge::rule_policy::{
    check_wire_rule, enable_problems, read_only_reason, validate_user_rule, PolicyProfile,
    RuleProblem,
};
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::UpstreamEffect;
use snitchwatch_proto::protocol::{Notification, Rule};
use std::sync::{Mutex as StdMutex, MutexGuard};

pub(crate) const NAME_TAKEN: &str = "A rule with that name already exists. Choose another name.";
pub(crate) const TCP_REFUSED: &str = "Adding, editing and renaming rules need the system \
     Snitchwatch service. In this per-user setup another program could pose as the firewall \
     service. You can still turn rules on or off and delete them.";
pub(crate) const BUSY: &str = "This rule is being changed. Try again in a moment.";
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
    /// A toggle or a delete: one command.
    Send(Notification),
    /// A new rule; its name stays busy until the daemon answers.
    Add { change: Notification, name: String },
    /// A change under the same name, restored if the daemon drops its file.
    Edit { change: Notification, old: Rule },
    /// `change` adds the new name, then the old rule is deleted.
    Rename { change: Notification, old: Rule },
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
    busy: &BusyNames,
) -> Result<Plan, Refusal> {
    let on_tcp = commands.transport() == DaemonTransport::Tcp;
    match command {
        Command::Delete { rule_id } => {
            if busy.contains(rule_id) {
                return Err(refusal("name", BUSY));
            }
            let effect = UpstreamEffect::DeleteRule {
                rule_id: rule_id.clone(),
            };
            notification(&effect).map(Plan::Send)
        }
        Command::Add { rule } => plan_add(rule, on_tcp, rules, busy),
        Command::Update { rule_id, rule } => plan_update(rule_id, rule, on_tcp, rules, busy),
    }
}

fn plan_add(
    rule: &serde_json::Value,
    on_tcp: bool,
    rules: &SharedRulesCache,
    busy: &BusyNames,
) -> Result<Plan, Refusal> {
    if on_tcp {
        return Err(refusal("rule", TCP_REFUSED));
    }
    let parsed = check_wire_rule(rule, PolicyProfile::Editor)?;
    name_free(&lock(rules), &parsed.name, busy)?;
    let effect = UpstreamEffect::AddRule { rule: rule.clone() };
    let change = notification(&effect)?;
    Ok(Plan::Add {
        change,
        name: parsed.name,
    })
}

fn plan_update(
    rule_id: &str,
    rule: &serde_json::Value,
    on_tcp: bool,
    rules: &SharedRulesCache,
    busy: &BusyNames,
) -> Result<Plan, Refusal> {
    let cache = lock(rules);
    let old = changeable(&cache, rule_id, busy)?;
    let effect = UpstreamEffect::UpdateRule {
        rule_id: rule_id.to_string(),
        rule: rule.clone(),
    };
    let change = notification(&effect)?;
    let new = change.rules[0].clone();
    let renamed = new.name != rule_id;
    if !renamed && only_enabled_differs(&old, &new) {
        if new.enabled && !old.enabled {
            may_turn_on(&new)?;
        }
        return Ok(Plan::Send(change));
    }
    if on_tcp {
        return Err(refusal("rule", TCP_REFUSED));
    }
    validate_user_rule(&new, PolicyProfile::Editor)?;
    if renamed {
        name_free(&cache, &new.name, busy)?;
        return Ok(Plan::Rename { change, old });
    }
    Ok(Plan::Edit { change, old })
}

/// Turning a rule on (re-review M2): not when it would match everything,
/// has an empty value or a duration the editor wouldn't write, whatever
/// made it. Turning one off is never checked.
fn may_turn_on(rule: &Rule) -> Result<(), Refusal> {
    let problems = enable_problems(rule);
    if problems.is_empty() {
        return Ok(());
    }
    Err(problems
        .into_iter()
        .map(|p| RuleProblem {
            reason: format!(
                "This rule can't be turned on from Snitchwatch: {}. Edit it first, if \
                 Snitchwatch can.",
                p.reason
            ),
            ..p
        })
        .collect())
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

/// A new name must be unused: not cached, not hidden, not being changed.
fn name_free(cache: &RulesCache, name: &str, busy: &BusyNames) -> Result<(), Refusal> {
    let rules = cache.rules().ok_or_else(|| refusal("rule", NOT_LOADED))?;
    if rules.contains_key(name) || cache.left_out().contains_key(name) {
        return Err(refusal("name", NAME_TAKEN));
    }
    if busy.contains(name) {
        return Err(refusal("name", BUSY));
    }
    Ok(())
}

/// The cached rule `rule_id` names, if Snitchwatch may change it.
fn changeable(cache: &RulesCache, rule_id: &str, busy: &BusyNames) -> Result<Rule, Refusal> {
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
    if busy.contains(rule_id) {
        return Err(refusal("name", BUSY));
    }
    Ok(old.clone())
}
