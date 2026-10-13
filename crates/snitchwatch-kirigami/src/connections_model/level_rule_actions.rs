//! The tree header rule button's handlers (Phase 2 step 3): what
//! `ConnectionsModel`'s `createLevelRule`, `deleteLevelRule` and
//! `pollLevelRules` invokables do, and the `levelRule*` data roles.
//!
//! The invokables stay thin in the parent module and call in here. What a
//! click may do is decided by plain functions ([`known_node`], [`delete_name`])
//! so it is tested without Qt. Nothing from QML is trusted: the node must be
//! one the model's grouped projection shows right now.

use core::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::{QString, QVariant};
use snitchwatch_bridge::ws_messages::ClientMessage;

use super::{qobject::ConnectionsModel, ROLE_LEVEL_RULE_REASON, ROLE_LEVEL_RULE_STATE};
use crate::connections::grouping::VisibleEntry;
use crate::connections::level_rules::{Level, NodeRef, Projection, RuleState};
use crate::make_rule::LevelRuleRefusal;

/// Why a rule button's command was not sent: one is already in flight.
const ALREADY_WORKING: &str = "Still waiting for the firewall to answer the last change here.";

/// Why a command was refused: the node isn't in the list (any more).
const NOT_IN_LIST: &str = "This row is no longer in the list.";

/// The longest host name or domain (bytes), as DNS allows.
const MAX_KEY_BYTES: usize = 253;

/// The longest program path (bytes), `PATH_MAX`.
const MAX_PROGRAM_BYTES: usize = 4096;

/// A header entry as the node its rule button is about; connections have none.
fn entry_node(entry: &VisibleEntry) -> Option<NodeRef<'_>> {
    match entry {
        VisibleEntry::ProcessHeader { key, .. } => Some(NodeRef {
            program: key,
            level: Level::Program,
            key: "",
        }),
        VisibleEntry::DomainHeader {
            process_key, key, ..
        } => Some(NodeRef {
            program: process_key,
            level: Level::Domain,
            key,
        }),
        VisibleEntry::HostHeader {
            process_key, key, ..
        } => Some(NodeRef {
            program: process_key,
            level: Level::Host,
            key,
        }),
        VisibleEntry::Row { .. } => None,
    }
}

/// Whether `node` (named by QML) is a header the projection shows.
fn known_node(entries: &[VisibleEntry], node: NodeRef<'_>) -> Result<(), &'static str> {
    let named = !node.program.is_empty()
        && node.program.len() <= MAX_PROGRAM_BYTES
        && node.key.len() <= MAX_KEY_BYTES
        && (node.level == Level::Program) == node.key.is_empty();
    let shown =
        |n: NodeRef<'_>| n.program == node.program && n.level == node.level && n.key == node.key;
    if named && entries.iter().filter_map(entry_node).any(shown) {
        Ok(())
    } else {
        Err(NOT_IN_LIST)
    }
}

/// The one rule a delete would remove, or why there is none to take.
fn delete_name(projection: &Projection) -> Result<String, &'static str> {
    match (projection.state, projection.names.first()) {
        (RuleState::Pending, _) => Err(ALREADY_WORKING),
        _ if projection.matching > 1 => Err(
            "There is more than one rule here, so Snitchwatch won't pick one. \
             Manage them on the Rules page.",
        ),
        (RuleState::Allow | RuleState::Deny, Some(name)) => Ok(name.clone()),
        (RuleState::Allow | RuleState::Deny, None) => {
            Err("Snitchwatch can't delete this rule. Manage it on the Rules page.")
        }
        _ => Err("There is no single rule here to delete. See the Rules page."),
    }
}

/// `node`'s projection, with the hosts under it when it is a domain.
fn project(model: &ConnectionsModel, node: NodeRef<'_>) -> Projection {
    let hosts = (node.level == Level::Domain)
        .then(|| model.grouping.domain_hosts(node.program, node.key))
        .into_iter()
        .flatten();
    model.level_rules.project_in(node, hosts)
}

/// Queue `message` for the live bridge session, whichever it is (a header
/// has no row, so no row session).
fn send_to_current_session(
    message: ClientMessage,
) -> Result<(), crate::bridge_runtime::SendClientMessageError> {
    crate::bridge_runtime::handles()
        .ok_or(crate::bridge_runtime::SendClientMessageError::Disconnected)?
        .try_send(message)
}

/// What a node's button shows: its projection, except that a node with no
/// rule is `Unavailable` when the Settings default duration can't make one.
/// `remembers` is asked only for such a node.
fn shown_state(
    projection: &Projection,
    remembers: impl FnOnce() -> bool,
) -> (RuleState, Option<LevelRuleRefusal>) {
    if projection.state == RuleState::None && !remembers() {
        return (
            RuleState::Unavailable,
            Some(LevelRuleRefusal::NotRemembered),
        );
    }
    (projection.state, projection.reason)
}

/// Whether the Settings default duration makes a rule that lasts.
fn default_duration_remembers() -> bool {
    let settings = crate::settings::read_settings(&crate::paths::settings_path());
    crate::pending_decision::parse_duration(settings.duration_token()).remembers()
}

/// The Settings default duration token, as a rule made now would take it.
pub(super) fn default_duration() -> &'static str {
    crate::settings::read_settings(&crate::paths::settings_path()).duration_token()
}

/// The `levelRuleState` / `levelRuleName` / `levelRuleReason` role of the
/// grouped row `row`.
pub(super) fn role(model: &ConnectionsModel, row: i32, role: i32) -> QVariant {
    let entry = model
        .grouped
        .then(|| model.grouped_projection.get(row as usize))
        .flatten();
    let projection = entry.and_then(entry_node).map(|node| project(model, node));
    let (state, reason) = match &projection {
        Some(p) => shown_state(p, default_duration_remembers),
        None => (RuleState::None, None),
    };
    let text = match role {
        ROLE_LEVEL_RULE_STATE => state.token().to_owned(),
        ROLE_LEVEL_RULE_REASON => reason.map(|r| r.reason().to_owned()).unwrap_or_default(),
        _ => projection
            .filter(|p| {
                matches!(
                    p.state,
                    RuleState::Allow | RuleState::Deny | RuleState::Pending
                )
            })
            .and_then(|p| p.names.first().cloned())
            .unwrap_or_default(),
    };
    QVariant::from(&QString::from(&text))
}

/// `createLevelRule`: the empty string when sent, else why not.
pub(super) fn create(
    mut model: Pin<&mut ConnectionsModel>,
    level: i32,
    process_key: &QString,
    key: &QString,
    choice: &QString,
    confirmed: bool,
) -> QString {
    use crate::make_rule::add_level_rule_message;
    let (process_key, key, choice) = (process_key.to_string(), key.to_string(), choice.to_string());
    let Some(level) = Level::from_i32(level) else {
        return QString::from(LevelRuleRefusal::UnknownLevel.reason());
    };
    let node = NodeRef {
        program: &process_key,
        level,
        key: &key,
    };
    if let Err(reason) = known_node(&model.grouped_projection, node) {
        return QString::from(reason);
    }
    let projection = project(&model, node);
    if projection.state == RuleState::Pending {
        return QString::from(ALREADY_WORKING);
    }
    if projection.too_broad {
        return QString::from(LevelRuleRefusal::TooBroad.reason());
    }
    let request_id = crate::rule_commands::next_request_id("level");
    let settings = crate::settings::read_settings(&crate::paths::settings_path());
    let request = match add_level_rule_message(
        node,
        &choice,
        &settings,
        confirmed,
        super::now_ms(),
        &request_id,
    ) {
        Ok(request) => request,
        Err(refusal) => return QString::from(refusal.reason()),
    };
    let operator = request.operator;
    let problem = crate::make_rule::send_problem(Some(request.message), send_to_current_session);
    if let Some(problem) = problem {
        return QString::from(problem);
    }
    model
        .as_mut()
        .rust_mut()
        .level_rules
        .begin_create(&request_id, node, operator);
    model.notify_level_rules_changed();
    QString::from("")
}

/// `deleteLevelRule`: the empty string when sent, else why not.
pub(super) fn delete(
    mut model: Pin<&mut ConnectionsModel>,
    level: i32,
    process_key: &QString,
    key: &QString,
) -> QString {
    let (process_key, key) = (process_key.to_string(), key.to_string());
    let Some(level) = Level::from_i32(level) else {
        return QString::from(LevelRuleRefusal::UnknownLevel.reason());
    };
    let node = NodeRef {
        program: &process_key,
        level,
        key: &key,
    };
    if let Err(reason) = known_node(&model.grouped_projection, node) {
        return QString::from(reason);
    }
    let name = match delete_name(&project(&model, node)) {
        Ok(name) => name,
        Err(reason) => return QString::from(reason),
    };
    let request_id = crate::rule_commands::next_request_id("level");
    let message = ClientMessage::DeleteRule {
        rule_id: name.clone(),
        request_id: Some(request_id.clone()),
        reply: None,
    };
    let problem = crate::make_rule::send_problem(Some(message), send_to_current_session);
    if let Some(problem) = problem {
        return QString::from(problem);
    }
    model
        .as_mut()
        .rust_mut()
        .level_rules
        .begin_delete(&request_id, node, &name);
    model.notify_level_rules_changed();
    QString::from("")
}

/// `pollLevelRules`: ends the commands that waited too long for an answer.
pub(super) fn poll(mut model: Pin<&mut ConnectionsModel>) {
    let ended = model
        .as_mut()
        .rust_mut()
        .level_rules
        .expire(std::time::Instant::now(), crate::make_rule::NO_ANSWER_AFTER);
    for finished in ended {
        model.as_mut().report_level_finished(finished);
    }
}

#[cfg(test)]
mod tests;
