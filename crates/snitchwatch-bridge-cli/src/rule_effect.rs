//! What a connected GUI's rule edit becomes: a command to the daemon, or an
//! undo of the GUI's optimistic change. Moved out of `lib.rs` unchanged.

use snitchwatch_bridge::cache::rules::{publish_rules, settle_rule_command, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::DaemonCommands;
use snitchwatch_bridge::translator::rule_notification::notification_for_effect;
use snitchwatch_bridge::translator::upstream::UpstreamEffect;
use snitchwatch_bridge::ws_messages::ServerMessage;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::RULE_COMMAND_TIMEOUT;

pub(crate) fn send(
    effect: &UpstreamEffect,
    commands: &DaemonCommands,
    rules: &SharedRulesCache,
    snapshot_tx: &broadcast::Sender<ServerMessage>,
) {
    // Rule enable/disable/delete: translate to a daemon
    // notification and send it down the outbound Notifications
    // stream(s); `DaemonCommands::send` assigns the id. The
    // rules cache follows the daemon's OK in reply order; a
    // spawned waiter re-broadcasts the unchanged list on any
    // other outcome, so the pump never blocks (#48). Anything
    // that isn't a rule edit yields `None` and falls through
    // to the original log line.
    match notification_for_effect(effect, 0) {
        Ok(Some(notification)) => {
            let action = notification.r#type;
            match commands.send(notification) {
                Ok(pending) => {
                    info!(id = pending.id(), action, "sent rule command to daemon");
                    tokio::spawn(settle_rule_command(
                        pending,
                        rules.clone(),
                        snapshot_tx.clone(),
                        RULE_COMMAND_TIMEOUT,
                    ));
                }
                // No daemon stream took it. Dropping is
                // correct — the daemon reloads its own rules on
                // connect, so there is nothing to replay. The
                // list is re-sent to undo the GUI's optimistic
                // change.
                Err(e) => {
                    warn!(action, error = %e, "rule command dropped");
                    publish_rules(rules, snapshot_tx);
                }
            }
        }
        Ok(None) => info!(?effect, "applied upstream effect"),
        // A rule the daemon would reject silently (see
        // `rule_from_wire`). Never send it, and re-send the
        // list to undo the GUI's optimistic change. The rule
        // body and name are GUI/daemon-supplied text: log only
        // the request kind and the name's length.
        Err(e) => {
            let (kind, name_len) = match effect {
                UpstreamEffect::AddRule { rule } => (
                    "add",
                    rule.get("name")
                        .and_then(|n| n.as_str())
                        .map_or(0, str::len),
                ),
                UpstreamEffect::UpdateRule { rule_id, .. } => ("update", rule_id.len()),
                UpstreamEffect::DeleteRule { rule_id } => ("delete", rule_id.len()),
                _ => ("other", 0),
            };
            error!(error = %e, kind, name_len, "refusing to send malformed rule to daemon");
            publish_rules(rules, snapshot_tx);
        }
    }
}
