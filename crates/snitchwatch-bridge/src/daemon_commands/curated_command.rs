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
//! [`check`](CuratedCommand::check) re-validates at the send point: a
//! `CHANGE_RULE` carries exactly one rule that passes
//! `curated::check_curated_rule` (the curated-specific allowlist plus the
//! rule editor's checks); a `DELETE_RULE` names one rule under the prefix.

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, PendingReply, SendError};
use crate::curated::{check_curated_rule, valid_id, CuratedEntry};
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

    /// `CHANGE_RULE` turning a curated rule on or off: `wanted` must be the
    /// daemon's `current` rule apart from `enabled`. Anything else, `None`.
    pub(crate) fn toggle(current: &Rule, wanted: &Rule) -> Option<Self> {
        let pure_toggle = current.name == wanted.name
            && crate::curated::reconcile::same_ignoring_enabled(current, wanted);
        (pure_toggle && check_curated_rule(wanted).is_ok()).then(|| Self::change(wanted.clone()))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curated::entries;

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
    }

    #[test]
    fn a_toggle_is_only_a_change_of_enabled() {
        let current = flatpak().rule();
        let off = Rule {
            enabled: false,
            ..current.clone()
        };
        let toggle = CuratedCommand::toggle(&current, &off).unwrap();
        assert_eq!(toggle.check(), Ok(()));
        assert!(!toggle.rule().unwrap().enabled);
        let mut wider = off.clone();
        wider.operator.as_mut().unwrap().list.remove(1);
        assert!(CuratedCommand::toggle(&current, &wider).is_none());
        let deny = Rule {
            action: "deny".into(),
            ..off
        };
        assert!(CuratedCommand::toggle(&current, &deny).is_none());
    }
}
