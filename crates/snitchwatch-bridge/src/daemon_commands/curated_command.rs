//! The send path for curated default rules (prompt-slot plan Part D).
//!
//! `DaemonCommands::send` refuses every name under the reserved
//! `snitchwatch-default-` prefix, so a GUI can't create a rule Snitchwatch's
//! reconcile would take for its own. The bridge's own curated rules go
//! through [`DaemonCommands::send_curated`] as a [`CuratedCommand`], whose
//! constructors are crate-private, like `BlocklistCommand`:
//!
//! ```compile_fail
//! let _ = snitchwatch_bridge::daemon_commands::CuratedCommand {
//!     notification: Default::default(),
//! };
//! ```
//!
//! A GUI may only turn a shipped entry's rule on or off
//! ([`DaemonCommands::send_curated_toggle`]), never add, edit or delete one.
//!
//! [`check`](CuratedCommand::check) re-validates at the send point: a
//! `CHANGE_RULE` carries exactly one rule that passes
//! `curated::check_curated_rule` (the curated-specific allowlist plus the
//! rule editor's checks); a `DELETE_RULE` names one rule under the prefix.

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, PendingReply, SendError};
use crate::curated::{check_curated_rule, entry_for_rule_name, toggleable, valid_id, CuratedEntry};
use crate::rule_name::{validate_rule_name, CURATED_DEFAULT_RULE_NAME_PREFIX};

/// A curated rule command built by the bridge itself.
#[derive(Debug, Clone)]
pub struct CuratedCommand {
    notification: Notification,
}

impl CuratedCommand {
    /// `CHANGE_RULE` installing `entry`'s rule.
    pub(crate) fn install(entry: &CuratedEntry) -> Self {
        Self::change(entry.rule())
    }

    /// `CHANGE_RULE` turning the daemon's rule `current` on or off: the data
    /// file's rule, with `current`'s `created`. `None` unless `current` is
    /// [`toggleable`] (that same rule apart from `enabled`).
    pub(crate) fn toggle(current: &Rule, enabled: bool) -> Option<Self> {
        if !toggleable(current) {
            return None;
        }
        let entry = entry_for_rule_name(&current.name)?;
        Some(Self::change(Rule {
            enabled,
            created: current.created,
            ..entry.rule()
        }))
    }

    fn change(rule: Rule) -> Self {
        Self {
            notification: Notification {
                r#type: Action::ChangeRule as i32,
                rules: vec![rule],
                ..Default::default()
            },
        }
    }

    /// `DELETE_RULE` for a name under the curated prefix; `None` otherwise.
    pub(crate) fn delete(name: &str) -> Option<Self> {
        if !curated_name(name) {
            return None;
        }
        Some(Self {
            notification: Notification {
                r#type: Action::DeleteRule as i32,
                rules: vec![Rule {
                    name: name.to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        })
    }

    /// The command's only rule.
    pub fn rule(&self) -> Option<&Rule> {
        self.notification.rules.first()
    }

    fn check(&self) -> Result<(), SendError> {
        let [rule] = self.notification.rules.as_slice() else {
            return Err(SendError::RefusedOperator);
        };
        validate_rule_name(&rule.name).map_err(|_| SendError::InvalidRuleName)?;
        if self.notification.r#type == Action::DeleteRule as i32 {
            return if curated_name(&rule.name) && rule.operator.is_none() {
                Ok(())
            } else {
                Err(SendError::RefusedOperator)
            };
        }
        if self.notification.r#type != Action::ChangeRule as i32 {
            return Err(SendError::NotAllowed);
        }
        check_curated_rule(rule).map_err(|_| SendError::RefusedOperator)
    }
}

fn curated_name(name: &str) -> bool {
    validate_rule_name(name).is_ok()
        && name
            .strip_prefix(CURATED_DEFAULT_RULE_NAME_PREFIX)
            .is_some_and(valid_id)
}

impl DaemonCommands {
    /// Send a curated rule command: the only path a rule under the curated
    /// prefix may take to the daemon.
    pub fn send_curated(&self, command: CuratedCommand) -> Result<PendingReply, SendError> {
        command.check()?;
        self.dispatch(command.notification)
    }

    /// A GUI's pure toggle of the curated rule `name` (plan item 13): the
    /// data file's rule, sent only while the daemon's cached copy is that
    /// rule apart from `enabled`. The GUI supplies nothing else, so it can't
    /// add, widen or keep alive any other rule under the prefix. [`SendError::ReservedName`] unless the cached rule is
    /// [`toggleable`].
    pub fn send_curated_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<PendingReply, SendError> {
        let command = {
            let cache = self.rules.cache();
            let cache = cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .rules()
                .and_then(|rules| rules.get(name))
                .and_then(|current| CuratedCommand::toggle(current, enabled))
        };
        self.send_curated(command.ok_or(SendError::ReservedName)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curated::entries;
    use crate::daemon_commands::{DaemonTransport, StreamRegistration};

    fn flatpak() -> &'static CuratedEntry {
        entries()
            .iter()
            .find(|e| e.id == "flatpak-flathub")
            .unwrap()
    }

    #[test]
    fn what_the_constructors_build_passes_the_send_check() {
        for entry in entries() {
            assert_eq!(CuratedCommand::install(entry).check(), Ok(()));
            let delete = CuratedCommand::delete(&entry.rule_name()).unwrap();
            assert_eq!(delete.check(), Ok(()));
        }
    }

    #[test]
    fn only_curated_names_and_exact_shapes_get_through() {
        assert!(CuratedCommand::delete("user-rule").is_none());
        assert!(CuratedCommand::delete("snitchwatch-default-../x").is_none());
        assert!(CuratedCommand::delete("z00-blocklist:ads:domains").is_none());
        // A rule widened after construction is refused at the send point.
        let mut widened = CuratedCommand::install(flatpak());
        widened.notification.rules[0].precedence = true;
        assert_eq!(widened.check(), Err(SendError::RefusedOperator));
        let mut renamed = CuratedCommand::install(flatpak());
        renamed.notification.rules[0].name = "my-own-rule".into();
        assert_eq!(renamed.check(), Err(SendError::RefusedOperator));
        let mut other_delete = CuratedCommand::delete(&flatpak().rule_name()).unwrap();
        other_delete.notification.rules[0].name = "user-rule".into();
        assert_eq!(other_delete.check(), Err(SendError::RefusedOperator));
    }

    #[test]
    fn a_toggle_changes_only_enabled_of_a_shipped_entrys_rule() {
        let current = flatpak().rule();
        let toggle = CuratedCommand::toggle(&current, false).unwrap();
        assert_eq!(toggle.check(), Ok(()));
        assert_eq!(
            toggle.rule(),
            Some(&Rule {
                enabled: false,
                ..current.clone()
            })
        );
        // Under the prefix but not in the data file: a squatter.
        let squatter = Rule {
            name: "snitchwatch-default-squatter".into(),
            ..current.clone()
        };
        assert!(CuratedCommand::toggle(&squatter, true).is_none());
        // Edited outside Snitchwatch, even within the curated shape: left
        // alone, so a toggle only ever sends a rule from the data file.
        let mut reshaped = current.clone();
        reshaped.operator.as_mut().unwrap().list.remove(1);
        assert!(CuratedCommand::toggle(&reshaped, true).is_none());
        let mut other_port = current;
        other_port.operator.as_mut().unwrap().list[2].data = "8443".into();
        assert!(check_curated_rule(&other_port).is_ok());
        assert!(CuratedCommand::toggle(&other_port, true).is_none());
    }

    fn connected(
        rules: Vec<Rule>,
    ) -> (
        DaemonCommands,
        tokio::sync::mpsc::Receiver<Notification>,
        StreamRegistration,
    ) {
        let sync = crate::cache::rules::RulesSync::new(tokio::sync::broadcast::channel(8).0);
        let commands = DaemonCommands::new(DaemonTransport::Unix, sync.clone());
        sync.stage(None, rules);
        let (stream, rx) = commands.open_stream(None);
        commands.on_reply(
            stream.id(),
            &snitchwatch_proto::protocol::NotificationReply {
                id: 0,
                code: snitchwatch_proto::protocol::NotificationReplyCode::Ok as i32,
                data: String::new(),
            },
        );
        (commands, rx, stream)
    }

    #[tokio::test]
    async fn a_gui_toggle_sends_the_daemons_own_rule_with_only_enabled_changed() {
        let mut daemons = flatpak().rule();
        daemons.created = 1_700_000_000;
        let squatter = Rule {
            name: "snitchwatch-default-squatter".into(),
            ..flatpak().rule()
        };
        let (commands, mut rx, _stream) = connected(vec![daemons.clone(), squatter]);
        let _reply = commands.send_curated_toggle(&daemons.name, false).unwrap();
        let sent = rx.try_recv().unwrap();
        assert_eq!(sent.r#type, Action::ChangeRule as i32);
        assert_eq!(
            sent.rules,
            [Rule {
                enabled: false,
                ..daemons
            }]
        );
        for name in [
            "snitchwatch-default-squatter",
            "snitchwatch-default-chronyc-local",
            "user-rule",
        ] {
            assert_eq!(
                commands.send_curated_toggle(name, true).err(),
                Some(SendError::ReservedName),
                "{name}"
            );
        }
        assert!(rx.try_recv().is_err(), "nothing else sent");
    }
}
