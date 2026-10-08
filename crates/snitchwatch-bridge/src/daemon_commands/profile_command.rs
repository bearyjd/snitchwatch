//! The send path for profile rules (issue #46 Part 2).
//!
//! `DaemonCommands::send` refuses every name under the profile prefix (a
//! GUI's allow there could replace a profile's deny). The bridge's own
//! profile rules go through [`DaemonCommands::send_profile`] as a
//! [`ProfileCommand`], a type with private fields whose constructors are
//! crate-private, like [`super::BlocklistCommand`]. A GUI message, or any
//! code outside this crate, can't produce one.
//!
//! ```compile_fail
//! let _ = snitchwatch_bridge::daemon_commands::ProfileCommand {
//!     notification: Default::default(),
//! };
//! ```
//!
//! [`check`](ProfileCommand::check) re-validates at the send point: a
//! `CHANGE_RULE` must be an enabled rule the bridge made (the prefix and the
//! profile tag) that passes the `ProfileRule` policy (`always`, no precedence, no
//! nolog, …); a `DELETE_RULE` names a rule under the prefix and nothing else.

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, PendingReply, SendError};
use crate::profiles::materializer::made_by_bridge;
use crate::rule_name::{is_reserved_profile_name, validate_rule_name};
use crate::rule_policy::{validate_user_rule, PolicyProfile};

/// A profile rule command built by the bridge itself.
#[derive(Debug, Clone)]
pub struct ProfileCommand {
    notification: Notification,
}

impl ProfileCommand {
    /// `CHANGE_RULE` installing a rule `profiles::materializer` built.
    pub(crate) fn install(rule: Rule) -> Self {
        Self {
            notification: Notification {
                r#type: Action::ChangeRule as i32,
                rules: vec![rule],
                ..Default::default()
            },
        }
    }

    /// `DELETE_RULE` for a name under the profile prefix; `None` for any
    /// other name.
    pub(crate) fn delete(name: &str) -> Option<Self> {
        if !is_reserved_profile_name(name) || validate_rule_name(name).is_err() {
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
        if !is_reserved_profile_name(&rule.name) {
            return Err(SendError::ReservedName);
        }
        if self.notification.r#type == Action::DeleteRule as i32 {
            return match rule.operator {
                None => Ok(()),
                Some(_) => Err(SendError::RefusedOperator),
            };
        }
        if self.notification.r#type != Action::ChangeRule as i32 {
            return Err(SendError::NotAllowed);
        }
        let shaped = rule.enabled && made_by_bridge(rule);
        if shaped && validate_user_rule(rule, PolicyProfile::ProfileRule).is_ok() {
            Ok(())
        } else {
            Err(SendError::RefusedOperator)
        }
    }
}

impl DaemonCommands {
    /// Send a profile rule command: the only path a rule under the profile
    /// prefix may take to the daemon; see the module doc.
    pub fn send_profile(&self, command: ProfileCommand) -> Result<PendingReply, SendError> {
        command.check()?;
        self.dispatch(command.notification)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::materializer::materialize_rule;
    use crate::profiles::store::ProfileRule;

    fn installable() -> Rule {
        let rule = ProfileRule {
            id: "r1".into(),
            action: "deny".into(),
            operand: "dest.host".into(),
            data: "x.example".into(),
            operator: None,
        };
        materialize_rule("home", &rule).unwrap()
    }

    #[test]
    fn what_the_materializer_builds_passes() {
        assert_eq!(ProfileCommand::install(installable()).check(), Ok(()));
        let delete = ProfileCommand::delete("850-profile:home:0000-r1").unwrap();
        assert_eq!(delete.check(), Ok(()));
    }

    #[test]
    fn a_rule_the_profile_policy_refuses_is_not_sent() {
        let refused = |rule: Rule| ProfileCommand::install(rule).check().err();
        let base = installable();
        assert!(refused(Rule {
            precedence: true,
            ..base.clone()
        })
        .is_some());
        assert!(refused(Rule {
            duration: "until restart".into(),
            ..base.clone()
        })
        .is_some());
        assert!(refused(Rule {
            nolog: true,
            ..base.clone()
        })
        .is_some());
        assert!(refused(Rule {
            description: String::new(),
            ..base.clone()
        })
        .is_some());
        assert!(refused(Rule {
            enabled: false,
            ..base.clone()
        })
        .is_some());
        assert_eq!(
            refused(Rule {
                name: "899-x".into(),
                ..base
            }),
            Some(SendError::ReservedName)
        );
    }

    #[test]
    fn only_profile_names_can_be_deleted() {
        for name in [
            "899-x",
            "z00-blocklist:ads:domains",
            "000-snitchwatch-x",
            "850-profile:a/b",
        ] {
            assert!(ProfileCommand::delete(name).is_none(), "{name}");
        }
    }
}
