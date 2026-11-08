//! The daemon version a [`DaemonCommands`] checks rules against before it
//! sends them (`crate::rule_policy::PolicyContext`): the version the gRPC
//! service last stored from a `ping` (`SharedDaemonVersion`). Every path that sends a rule holds a
//! `DaemonCommands`, so each reads the same version at the moment it checks.

use snitchwatch_proto::protocol::{Action, Notification};

use tracing::warn;

use super::{lock, ConnKey, DaemonCommands, Inner};
use crate::daemon_config::{DaemonVersion, SharedDaemonVersion};
use crate::rule_policy::{validate_operator_in, PolicyContext};

impl DaemonCommands {
    /// Check rules against the version `version` holds. Without this, a
    /// handle of its own that stays unknown: v1.8.0's rules.
    pub fn with_daemon_version(mut self, version: SharedDaemonVersion) -> Self {
        self.daemon_version = version;
        self
    }

    /// The version this handle checks rules against.
    pub fn daemon_version(&self) -> SharedDaemonVersion {
        self.daemon_version.clone()
    }

    /// What a `ping` from `peer` reported, stored (see
    /// [`SharedDaemonVersion::observe`], whose return this is) only when
    /// `peer` is the connection of the current `Notifications` stream, or
    /// when no stream is current. A second TCP peer pinging another version
    /// therefore can't move a known one. On the Unix socket both keys are
    /// `None` (every peer is root), so its pings are always taken. Checked
    /// under the streams lock, so a stream can't become current in between.
    pub fn observe_daemon_version(&self, peer: ConnKey, version: Option<DaemonVersion>) -> bool {
        let inner = lock(&self.inner);
        let holder = inner.current.and_then(|id| inner.streams.get(&id));
        if let Some(holder) = holder.filter(|open| open.conn != peer) {
            warn!(
                ?peer,
                holder = ?holder.conn,
                "ignoring the daemon version in a ping from a peer that does not hold the stream"
            );
            return false;
        }
        self.daemon_version.observe(version)
    }

    /// Called by `close_stream` once the closed stream was current: with no
    /// stream current any more, the version is forgotten, so a daemon that
    /// comes back as another version isn't judged by the old one until its
    /// first `ping` (unknown means v1.8.0's rules). Nothing is reported as
    /// changed: the next readable `ping` is a change from unknown and
    /// triggers the recheck then.
    pub(super) fn forget_version_without_current(&self, inner: &Inner) {
        if inner.current.is_none() {
            self.daemon_version.set(None);
        }
    }

    /// The checks for the daemon's version as last reported.
    pub fn policy_context(&self) -> PolicyContext {
        PolicyContext::for_daemon(self.daemon_version.get())
    }

    /// Whether [`Self::send`] refuses `notification` for its conditions: a
    /// `CHANGE_RULE` with a rule that has no operator, or an enabled one
    /// [`validate_operator_in`] refuses for the connected daemon (the daemon
    /// does not compile disabled rules, so turning one off stays allowed).
    pub(super) fn refuses_conditions(&self, notification: &Notification) -> bool {
        let context = self.policy_context();
        notification.r#type == Action::ChangeRule as i32
            && notification.rules.iter().any(|rule| {
                rule.operator.as_ref().is_none_or(|op| {
                    validate_operator_in(op, context.for_rule(rule.enabled)).is_err()
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::rules::RulesSync;
    use crate::daemon_commands::{DaemonTransport, SendError};
    use snitchwatch_proto::protocol::{Operator, Rule};
    use tokio::sync::broadcast;

    fn change(operand: &str, enabled: bool) -> Notification {
        Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![Rule {
                name: "lan-in".into(),
                enabled,
                action: "allow".into(),
                duration: "always".into(),
                operator: Some(Operator {
                    r#type: "network".into(),
                    operand: operand.into(),
                    data: "192.168.0.0/16".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn send_refuses_source_network_only_once_a_1_9_daemon_reported() {
        let version = SharedDaemonVersion::default();
        let commands = DaemonCommands::new(
            DaemonTransport::Tcp,
            RulesSync::new(broadcast::channel(8).0),
        )
        .with_daemon_version(version.clone());
        // No stream is open: a command that passes the checks finds no
        // daemon, one that fails them is refused first.
        let sent = |operand: &str| commands.send(change(operand, true)).err();
        assert_eq!(sent("source.network"), Some(SendError::NoDaemon));
        version.set(DaemonVersion::parse("1.8.0"));
        assert_eq!(sent("source.network"), Some(SendError::NoDaemon));
        version.set(DaemonVersion::parse("v1.9.0-rc1"));
        assert_eq!(sent("source.network"), Some(SendError::RefusedOperator));
        assert_eq!(sent("dest.network"), Some(SendError::NoDaemon));
        assert!(commands.policy_context().refuse_source_network);
        assert!(commands.clone().policy_context().refuse_source_network);
        version.set(None);
        assert_eq!(sent("source.network"), Some(SendError::NoDaemon));
    }

    #[test]
    fn a_1_9_daemon_accepts_turning_a_source_network_rule_off_but_not_on() {
        let version = SharedDaemonVersion::default();
        version.set(DaemonVersion::parse("1.9.0"));
        let commands = DaemonCommands::new(
            DaemonTransport::Tcp,
            RulesSync::new(broadcast::channel(8).0),
        )
        .with_daemon_version(version);
        // The daemon skips `Compile()` for a disabled rule, so turning one
        // off is safe; turning it on would make the daemon refuse to load it.
        assert!(!commands.refuses_conditions(&change("source.network", false)));
        assert!(commands.refuses_conditions(&change("source.network", true)));
        assert_eq!(
            commands.send(change("source.network", true)).err(),
            Some(SendError::RefusedOperator)
        );
    }

    #[test]
    fn a_disabled_rule_still_gets_the_checks_every_daemon_applies() {
        // A disabled rule is saved and can be turned on outside the bridge, so
        // the pairings that crash every daemon stay refused for it.
        let mut crashing = change("dest.network", false);
        crashing.rules[0].operator.as_mut().unwrap().r#type = "regexp".into();
        for version in [None, DaemonVersion::parse("1.9.0")] {
            let shared = SharedDaemonVersion::default();
            shared.set(version);
            let commands = DaemonCommands::new(
                DaemonTransport::Tcp,
                RulesSync::new(broadcast::channel(8).0),
            )
            .with_daemon_version(shared);
            assert!(commands.refuses_conditions(&crashing));
        }
    }

    #[test]
    fn a_handle_without_a_version_keeps_1_8_rules() {
        let commands = DaemonCommands::new(
            DaemonTransport::Unix,
            RulesSync::new(broadcast::channel(8).0),
        );
        assert_eq!(commands.policy_context(), PolicyContext::default());
        assert!(!commands.refuses_conditions(&change("source.network", false)));
    }
}
