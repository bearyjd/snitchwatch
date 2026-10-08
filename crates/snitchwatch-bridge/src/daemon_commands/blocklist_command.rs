//! The one exception to the send point's rule policy (issue #45 PR B).
//!
//! `DaemonCommands::send` refuses every `lists` operator (it makes root read
//! a directory) and every name under a blocklist prefix (an allow there
//! could replace a blocklist's deny). The bridge's own blocklist rules need
//! both, so they go through [`DaemonCommands::send_blocklist`] as a
//! [`BlocklistCommand`]: a type with private fields whose constructors are
//! crate-private and build exactly the shape the blocklist code installs. A
//! GUI message, or any code outside this crate, can't produce one.
//!
//! ```compile_fail
//! // Neither a struct literal nor the constructors are reachable outside.
//! let _ = snitchwatch_bridge::daemon_commands::BlocklistCommand {
//!     notification: Default::default(),
//! };
//! ```
//!
//! [`check`](BlocklistCommand::check) re-validates the shape at the send
//! point anyway, so a later change to the constructors can't widen it.

use std::path::{Component, Path};

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, PendingReply, SendError};
use crate::blocklists::list_dir::{IdComponent, ListDir, LISTS_DIR_NAME};
use crate::blocklists::materializer::{list_rule_name, materialize_list_rule, ListKind};
use crate::rule_name::{
    is_reserved_blocklist_name, validate_rule_name, BLOCKLIST_RULE_NAME_PREFIX,
};

/// A blocklist rule command built by the bridge itself.
#[derive(Debug, Clone)]
pub struct BlocklistCommand {
    notification: Notification,
}

impl BlocklistCommand {
    /// `CHANGE_RULE` installing the deny rule for one kind of one list.
    pub(crate) fn install(list: &IdComponent, kind: ListKind, dir: &ListDir) -> Self {
        Self::change(materialize_list_rule(list, kind, &dir.kind_dir(list, kind)).into())
    }

    /// `CHANGE_RULE` for a rule [`install`](Self::install) built earlier.
    pub(crate) fn change(rule: Rule) -> Self {
        Self {
            notification: Notification {
                r#type: Action::ChangeRule as i32,
                rules: vec![rule],
                ..Default::default()
            },
        }
    }

    /// `DELETE_RULE` for a name under a blocklist prefix; `None` for any
    /// other name.
    pub(crate) fn delete(name: &str) -> Option<Self> {
        if !is_reserved_blocklist_name(name) || validate_rule_name(name).is_err() {
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

    /// Exactly what the constructors build, or an error.
    fn check(&self) -> Result<(), SendError> {
        let [rule] = self.notification.rules.as_slice() else {
            return Err(SendError::RefusedOperator);
        };
        validate_rule_name(&rule.name).map_err(|_| SendError::InvalidRuleName)?;
        if self.notification.r#type == Action::DeleteRule as i32 {
            return if is_reserved_blocklist_name(&rule.name) && rule.operator.is_none() {
                Ok(())
            } else {
                Err(SendError::RefusedOperator)
            };
        }
        if self.notification.r#type != Action::ChangeRule as i32 {
            return Err(SendError::NotAllowed);
        }
        let shape_ok = rule.name.starts_with(BLOCKLIST_RULE_NAME_PREFIX)
            && rule.action == "deny"
            && rule.duration == "always"
            && rule.enabled
            && !rule.precedence;
        let operator_ok = rule.operator.as_ref().is_some_and(|op| {
            op.r#type == "lists"
                && op.list.is_empty()
                && !op.sensitive
                && ListKind::from_operand(&op.operand)
                    .is_some_and(|kind| data_matches(&op.data, kind, &rule.name))
        });
        if shape_ok && operator_ok {
            Ok(())
        } else {
            Err(SendError::RefusedOperator)
        }
    }
}

/// `data` is `/…/blocklists/<list>/<kind>`: absolute, normal components
/// only, no trailing slash, and the rule is named for that list and kind.
fn data_matches(data: &str, kind: ListKind, name: &str) -> bool {
    let Some(relative) = data.strip_prefix('/') else {
        return false;
    };
    // No empty, `.` or `..` segment: no trailing or doubled slash either.
    if !relative
        .split('/')
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    {
        return false;
    }
    let path = Path::new(data);
    if !path
        .components()
        .skip(1)
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return false;
    }
    let names: Vec<&str> = path
        .iter()
        .rev()
        .take(3)
        .filter_map(|c| c.to_str())
        .collect();
    let [kind_dir, list, root] = names.as_slice() else {
        return false;
    };
    *kind_dir == kind.dir_name()
        && *root == LISTS_DIR_NAME
        && IdComponent::parse(list).is_some_and(|list| list_rule_name(&list, kind) == name)
}

impl DaemonCommands {
    /// Send a blocklist rule command. The only path a `lists` operator or a
    /// blocklist rule name may take to the daemon; see the module doc.
    pub fn send_blocklist(&self, command: BlocklistCommand) -> Result<PendingReply, SendError> {
        command.check()?;
        self.dispatch(command.notification)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, ListDir) {
        let state = tempfile::tempdir().unwrap();
        let dir = ListDir::open(&state.path().canonicalize().unwrap()).unwrap();
        (state, dir)
    }

    fn installed() -> (tempfile::TempDir, BlocklistCommand) {
        let (state, dir) = dir();
        let list = IdComponent::from_id("ads");
        (
            state,
            BlocklistCommand::install(&list, ListKind::Domains, &dir),
        )
    }

    #[test]
    fn what_the_constructors_build_passes() {
        let (_state, dir) = dir();
        for id in ["ads", "a/b", &"x".repeat(200)] {
            let list = IdComponent::from_id(id);
            for kind in ListKind::ALL {
                assert_eq!(BlocklistCommand::install(&list, kind, &dir).check(), Ok(()));
            }
        }
        assert!(BlocklistCommand::delete("z00-blocklist:ads:domains").is_some());
        assert!(BlocklistCommand::delete("900-blocklist:ads:0001-x").is_some());
    }

    #[test]
    fn delete_is_only_for_blocklist_names() {
        for name in ["899-firefox", "z00-blocklisted", "z00-blocklist:../x", ""] {
            assert!(BlocklistCommand::delete(name).is_none(), "{name}");
        }
    }

    #[test]
    fn every_widened_shape_is_refused() {
        type Tamper = fn(&mut Rule);
        let tampers: [(&str, Tamper); 12] = [
            ("allow", |r| r.action = "allow".into()),
            ("temporary", |r| r.duration = "5m".into()),
            ("disabled", |r| r.enabled = false),
            ("precedence", |r| r.precedence = true),
            ("user name", |r| r.name = "899-x".into()),
            ("other list", |r| {
                r.name = "z00-blocklist:other:domains".into()
            }),
            ("other kind", |r| r.name = "z00-blocklist:ads:ips".into()),
            ("simple", |r| {
                r.operator.as_mut().unwrap().r#type = "simple".into()
            }),
            ("nets", |r| {
                r.operator.as_mut().unwrap().operand = "lists.nets".into()
            }),
            ("etc", |r| r.operator.as_mut().unwrap().data = "/etc".into()),
            ("dotdot", |r| {
                let op = r.operator.as_mut().unwrap();
                op.data = format!("{}/../../blocklists/ads/domains", op.data);
            }),
            ("relative", |r| {
                r.operator.as_mut().unwrap().data = "blocklists/ads/domains".into()
            }),
        ];
        for (what, tamper) in tampers {
            let (_state, mut command) = installed();
            tamper(&mut command.notification.rules[0]);
            assert!(command.check().is_err(), "{what} passed");
        }
        let (_state, mut command) = installed();
        command
            .notification
            .rules
            .push(command.notification.rules[0].clone());
        assert!(command.check().is_err(), "two rules passed");
        let (_state, mut command) = installed();
        command.notification.r#type = Action::ChangeConfig as i32;
        assert!(command.check().is_err(), "CHANGE_CONFIG passed");
        let (_state, mut command) = installed();
        let data = &mut command.notification.rules[0]
            .operator
            .as_mut()
            .unwrap()
            .data;
        data.push('/');
        assert!(command.check().is_err(), "a trailing slash passed");
    }
}
