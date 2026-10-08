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
//! point anyway, so a later change to the constructors can't widen it, and
//! pins `data` to the list root the sink set once with
//! [`DaemonCommands::pin_blocklist_root`]; before that, no install is sent.
//! A delete names no path, so it needs no root: a bridge without a state
//! directory can still remove the blocklist rules it once made (issue #73).

use std::path::{Component, Path, PathBuf};

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, Delivery, PendingReply, SendError};
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

    fn change(rule: Rule) -> Self {
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

    /// Exactly what the constructors build for the list root `root`, or an
    /// error.
    fn check(&self, root: Option<&Path>) -> Result<(), SendError> {
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
        // An install is only ever for the pinned root.
        let root = root.ok_or(SendError::RefusedOperator)?;
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
                    .is_some_and(|kind| data_matches(&op.data, kind, &rule.name, root))
        });
        if shape_ok && operator_ok {
            Ok(())
        } else {
            Err(SendError::RefusedOperator)
        }
    }
}

/// `data` is exactly `<root>/<list>/<kind>`: absolute, normal components
/// only, no trailing slash, under the pinned root, and the rule is named for
/// that list and kind.
fn data_matches(data: &str, kind: ListKind, name: &str, root: &Path) -> bool {
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
    let Ok(below_root) = path.strip_prefix(root) else {
        return false;
    };
    let names: Vec<&str> = below_root.iter().filter_map(|c| c.to_str()).collect();
    let [list, kind_dir] = names.as_slice() else {
        return false;
    };
    root.file_name().is_some_and(|n| n == LISTS_DIR_NAME)
        && *kind_dir == kind.dir_name()
        && IdComponent::parse(list).is_some_and(|list| list_rule_name(&list, kind) == name)
}

impl DaemonCommands {
    /// Pin the list directory root every [`BlocklistCommand`] must point
    /// under. Set once; a second, different root is refused (and returned).
    pub(crate) fn pin_blocklist_root(&self, root: &Path) -> Result<(), PathBuf> {
        let pinned = self.blocklist_root.get_or_init(|| root.to_path_buf());
        if pinned == root {
            Ok(())
        } else {
            Err(pinned.clone())
        }
    }

    /// Send a blocklist rule command. The only path a `lists` operator or a
    /// blocklist rule name may take to the daemon; see the module doc.
    /// No install is sent before [`pin_blocklist_root`](Self::pin_blocklist_root).
    pub fn send_blocklist(&self, command: BlocklistCommand) -> Result<PendingReply, SendError> {
        command.check(self.blocklist_root.get().map(PathBuf::as_path))?;
        self.dispatch(command.notification)
    }

    /// Send the delete of a blocklist rule that was read from the daemon's
    /// committed rule snapshot (issue #73), to the stream that snapshot came
    /// from and to no other. Never over the TCP transport: there any
    /// local process can pose as the daemon, show a list of its own as the
    /// snapshot, and have a delete of a real rule's name sent on to the
    /// real daemon. Refused until #35 retires that transport.
    pub fn send_leftover_delete(
        &self,
        command: BlocklistCommand,
    ) -> Result<PendingReply, SendError> {
        command.check(None)?;
        if command.notification.r#type != Action::DeleteRule as i32 {
            return Err(SendError::RefusedOperator);
        }
        self.dispatch_via(command.notification, Delivery::CommittedStream)
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

    fn installed() -> (tempfile::TempDir, ListDir, BlocklistCommand) {
        let (state, dir) = dir();
        let list = IdComponent::from_id("ads");
        let command = BlocklistCommand::install(&list, ListKind::Domains, &dir);
        (state, dir, command)
    }

    #[test]
    fn what_the_constructors_build_passes() {
        let (_state, dir) = dir();
        for id in ["ads", "a/b", &"x".repeat(200)] {
            let list = IdComponent::from_id(id);
            for kind in ListKind::ALL {
                let command = BlocklistCommand::install(&list, kind, &dir);
                assert_eq!(command.check(Some(dir.root())), Ok(()));
            }
        }
        for name in ["z00-blocklist:ads:domains", "900-blocklist:ads:0001-x"] {
            let delete = BlocklistCommand::delete(name).unwrap();
            assert_eq!(delete.check(Some(dir.root())), Ok(()));
        }
    }

    #[test]
    fn delete_is_only_for_blocklist_names() {
        for name in ["899-firefox", "z00-blocklisted", "z00-blocklist:../x", ""] {
            assert!(BlocklistCommand::delete(name).is_none(), "{name}");
        }
    }

    /// Review L1: the data path is pinned to the sink's own root, not just
    /// its last three components.
    #[test]
    fn a_rule_for_another_root_is_refused() {
        let (_state, _dir, command) = installed();
        let (_other_state, other) = dir();
        assert!(command.check(Some(other.root())).is_err());
        assert!(command
            .check(Some(Path::new("/var/lib/snitchwatch/blocklists")))
            .is_err());
        assert!(command.check(None).is_err(), "an install needs a root");
    }

    #[test]
    fn every_widened_shape_is_refused() {
        type Tamper = fn(&mut Rule);
        let tampers: [(&str, Tamper); 13] = [
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
            ("same tail, other root", |r| {
                r.operator.as_mut().unwrap().data = "/tmp/x/blocklists/ads/domains".into()
            }),
        ];
        for (what, tamper) in tampers {
            let (_state, dir, mut command) = installed();
            tamper(&mut command.notification.rules[0]);
            assert!(command.check(Some(dir.root())).is_err(), "{what} passed");
        }
        let (_state, dir, mut command) = installed();
        command
            .notification
            .rules
            .push(command.notification.rules[0].clone());
        assert!(command.check(Some(dir.root())).is_err(), "two rules passed");
        let (_state, dir, mut command) = installed();
        command.notification.r#type = Action::ChangeConfig as i32;
        assert!(
            command.check(Some(dir.root())).is_err(),
            "CHANGE_CONFIG passed"
        );
        let (_state, dir, mut command) = installed();
        let data = &mut command.notification.rules[0]
            .operator
            .as_mut()
            .unwrap()
            .data;
        data.push('/');
        assert!(
            command.check(Some(dir.root())).is_err(),
            "a trailing slash passed"
        );
    }

    // --- send_leftover_delete (issue #73, security L1) -----------------------

    use crate::cache::rules::RulesSync;
    use crate::daemon_commands::DaemonTransport;
    use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode};
    use tokio::sync::broadcast;

    fn commands(transport: DaemonTransport) -> DaemonCommands {
        DaemonCommands::new(transport, RulesSync::new(broadcast::channel(8).0))
    }

    fn hello() -> NotificationReply {
        NotificationReply {
            id: 0,
            code: NotificationReplyCode::Ok as i32,
            ..Default::default()
        }
    }

    fn the_delete() -> BlocklistCommand {
        BlocklistCommand::delete("z00-blocklist:ads:domains").unwrap()
    }

    /// Over TCP any local process can say HELLO and show a snapshot of its
    /// own, so a delete read from one is never sent.
    #[tokio::test]
    async fn a_leftover_delete_is_never_sent_over_tcp() {
        let commands = commands(DaemonTransport::Tcp);
        let (stream, _rx) = commands.open_stream(None);
        commands.on_reply(stream.id(), &hello());
        assert!(matches!(
            commands.send_leftover_delete(the_delete()),
            Err(SendError::NotOnThisTransport)
        ));
    }

    /// On the Unix socket it goes to the stream whose snapshot the cache
    /// holds; a stream that said HELLO without one withdraws that snapshot,
    /// and then nothing is sent at all.
    #[tokio::test]
    async fn a_leftover_delete_goes_only_to_the_stream_whose_snapshot_is_held() {
        let rules = RulesSync::new(broadcast::channel(8).0);
        let commands = DaemonCommands::new(DaemonTransport::Unix, rules.clone());
        rules.stage(None, Vec::new());
        let (first, mut first_rx) = commands.open_stream(None);
        commands.on_reply(first.id(), &hello());
        assert!(commands.send_leftover_delete(the_delete()).is_ok());
        assert!(first_rx.try_recv().is_ok(), "the committed stream got it");

        let (second, mut second_rx) = commands.open_stream(None);
        commands.on_reply(second.id(), &hello());
        assert!(matches!(
            commands.send_leftover_delete(the_delete()),
            Err(SendError::NoDaemon)
        ));
        assert!(second_rx.try_recv().is_err());
        assert!(first_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn only_a_delete_can_be_sent_that_way() {
        let (_state, _dir, install) = installed();
        let commands = commands(DaemonTransport::Unix);
        assert!(commands.send_leftover_delete(install).is_err());
    }
}
