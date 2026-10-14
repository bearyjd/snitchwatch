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
//! `CHANGE_RULE` carries exactly one rule, a list entry's own (apart from
//! `enabled` and `created`), that also passes `curated::check_curated_rule`
//! (the curated-specific allowlist plus the rule editor's checks), or a copy
//! from before its sender pin being turned off (#120 36/41); a
//! `DELETE_RULE` names one rule under the prefix.

use snitchwatch_proto::protocol::{Action, Notification, Rule};

use super::{DaemonCommands, PendingReply, SendError};
use crate::curated::canonical::{is_legacy, is_unedited};
use crate::curated::{check_curated_rule, entry_for_rule_name, toggleable, valid_id, CuratedEntry};
use crate::rule_name::{validate_rule_name, CURATED_DEFAULT_RULE_NAME_PREFIX};

/// A curated rule command built by the bridge itself.
#[derive(Debug, Clone)]
pub struct CuratedCommand {
    notification: Notification,
    broad_authorized: bool,
}

impl CuratedCommand {
    /// `CHANGE_RULE` installing `entry`'s rule.
    pub(crate) fn install(entry: &CuratedEntry) -> Self {
        Self::install_as(entry, None)
    }

    /// `CHANGE_RULE` installing `entry`'s rule, a run-time account's pinned
    /// to `account_uid` (without one, the send check refuses it).
    pub(crate) fn install_as(entry: &CuratedEntry, account_uid: Option<u32>) -> Self {
        Self::change(entry.rule_as(account_uid))
    }

    /// `CHANGE_RULE` replacing `current`, `entry`'s copy from before its
    /// sender pin, with the pinned rule at `account_uid`, keeping
    /// `current`'s `enabled` and `created` (#120 36/41). `None` unless
    /// `current` is that copy.
    pub(crate) fn upgrade(
        entry: &CuratedEntry,
        current: &Rule,
        account_uid: Option<u32>,
    ) -> Option<Self> {
        is_legacy(entry, current).then(|| {
            Self::change(Rule {
                enabled: current.enabled,
                created: current.created,
                ..entry.rule_as(account_uid)
            })
        })
    }

    /// `CHANGE_RULE` turning the daemon's rule `current` on or off: the data
    /// file's rule (at the uid `current` names, for a run-time account),
    /// with `current`'s `created`. `None` unless `current` is [`toggleable`]
    /// (that same rule apart from `enabled`). A copy from before the
    /// entry's sender pin may only be turned off, as it is.
    pub(crate) fn toggle(current: &Rule, enabled: bool) -> Option<Self> {
        if !toggleable(current) {
            return None;
        }
        let entry = entry_for_rule_name(&current.name)?;
        let rule = if is_unedited(Some(entry), None, current) {
            entry.rule_matching(current)?
        } else if !enabled {
            entry.legacy_rule()?
        } else {
            return None;
        };
        Some(Self::change(Rule {
            enabled,
            created: current.created,
            ..rule
        }))
    }

    pub(crate) fn authorized_install(entry: &CuratedEntry) -> Self {
        let mut command = Self::install(entry);
        command.broad_authorized = true;
        command
    }

    pub(crate) fn authorized_toggle(current: &Rule) -> Option<Self> {
        let mut command = Self::toggle(current, true)?;
        command.broad_authorized = true;
        Some(command)
    }

    fn change(rule: Rule) -> Self {
        Self {
            broad_authorized: false,
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
            broad_authorized: false,
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
        // Exactly a list entry's rule (security review L2), and the shape;
        // or its copy from before the sender pin, turned off.
        let exact = entry_for_rule_name(&rule.name).is_some_and(|entry| {
            is_unedited(Some(entry), None, rule) || (!rule.enabled && is_legacy(entry, rule))
        });
        if !exact {
            return Err(SendError::RefusedOperator);
        }
        if rule.enabled
            && entry_for_rule_name(&rule.name).is_some_and(|entry| entry.broad())
            && !self.broad_authorized
        {
            return Err(SendError::ReservedName);
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
        if command.rule().is_some_and(|rule| {
            rule.enabled && entry_for_rule_name(&rule.name).is_some_and(|entry| entry.broad())
        }) {
            return Err(SendError::ReservedName);
        }
        self.dispatch(command.notification)
    }

    /// Final broad admission; caller holds its current choice/revision guard.
    pub(crate) fn send_curated_guarded(
        &self,
        command: CuratedCommand,
        generation: u64,
        reenable: bool,
    ) -> Result<PendingReply, SendError> {
        command.check()?;
        self.dispatch_via_checked(
            command.notification,
            super::Delivery::Usual,
            Some((generation, if reenable { Some(true) } else { None })),
        )
    }

    /// A GUI's pure toggle of the curated rule `name` (plan item 13): the
    /// data file's rule, sent only while the daemon's cached copy is that
    /// rule apart from `enabled`. The GUI supplies nothing else, so it can't
    /// add, widen or keep alive any other rule under the prefix.
    /// [`SendError::ReservedName`] unless the cached rule is [`toggleable`].
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
        let command = command.ok_or(SendError::ReservedName)?;
        if entry_for_rule_name(name).is_some_and(|entry| entry.broad()) {
            if enabled {
                return Err(SendError::ReservedName);
            }
            command.check()?;
            let generation = *self.ready.borrow();
            return self.dispatch_via_checked(
                command.notification,
                super::Delivery::Usual,
                Some((generation, Some(false))),
            );
        }
        self.send_curated(command)
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
            if entry.account().is_some() {
                // A run-time account's rule without its uid is never sent.
                let unresolved = CuratedCommand::install(entry).check();
                assert_eq!(unresolved, Err(SendError::RefusedOperator));
            }
            assert_eq!(
                CuratedCommand::install_as(entry, Some(991)).check(),
                if entry.broad() {
                    Err(SendError::ReservedName)
                } else {
                    Ok(())
                }
            );
            if entry.account().is_none() {
                assert_eq!(CuratedCommand::authorized_install(entry).check(), Ok(()));
            }
            let delete = CuratedCommand::delete(&entry.rule_name()).unwrap();
            assert_eq!(delete.check(), Ok(()));
        }
    }

    /// #120 36/41: a copy from before the sender pin is replaced keeping its
    /// `enabled` and `created`, and may be turned off as it is, never on.
    #[test]
    fn a_copy_from_before_the_sender_pin_is_replaced_or_turned_off() {
        let nm = entries()
            .iter()
            .find(|e| e.id == "networkmanager-connectivity-check")
            .unwrap();
        let old = Rule {
            enabled: false,
            created: 7,
            ..nm.legacy_rule().unwrap()
        };
        let upgrade = CuratedCommand::upgrade(nm, &old, None).unwrap();
        assert_eq!(upgrade.check(), Ok(()));
        let sent = upgrade.rule().unwrap();
        assert_eq!((sent.enabled, sent.created), (false, 7));
        assert!(crate::curated::canonical::is_unedited(Some(nm), None, sent));
        assert!(CuratedCommand::upgrade(nm, &nm.rule(), None).is_none());

        let on = Rule {
            enabled: true,
            ..old.clone()
        };
        let off = CuratedCommand::toggle(&on, false).unwrap();
        assert_eq!(off.check(), Ok(()));
        assert!(is_legacy(nm, off.rule().unwrap()));
        assert!(CuratedCommand::toggle(&on, true).is_none());
        assert!(CuratedCommand::toggle(&old, true).is_none());
        // The old form turned on is never sent.
        let mut on_again = off.clone();
        on_again.notification.rules[0].enabled = true;
        assert_eq!(on_again.check(), Err(SendError::RefusedOperator));
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
        // Security review L2: still the curated shape, but not the list's
        // rule (another port, or another entry's name), is refused too.
        let mut other_port = CuratedCommand::install(flatpak());
        other_port.notification.rules[0]
            .operator
            .as_mut()
            .unwrap()
            .list[2]
            .data = "8443".into();
        assert!(check_curated_rule(other_port.rule().unwrap()).is_ok());
        assert_eq!(other_port.check(), Err(SendError::RefusedOperator));
        let mut unlisted = CuratedCommand::install(flatpak());
        unlisted.notification.rules[0].name = "snitchwatch-default-unlisted".into();
        assert_eq!(unlisted.check(), Err(SendError::RefusedOperator));
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

    #[tokio::test]
    async fn generic_curated_send_refuses_even_authorized_broad_constructors() {
        let dns = entries()
            .iter()
            .find(|entry| entry.id == "dns-resolved")
            .unwrap();
        let mut disabled = dns.rule();
        disabled.enabled = false;
        let (commands, mut rx, _stream) = connected(vec![disabled.clone()]);
        assert_eq!(
            commands
                .send_curated(CuratedCommand::authorized_install(dns))
                .err(),
            Some(SendError::ReservedName)
        );
        assert_eq!(
            commands
                .send_curated(CuratedCommand::authorized_toggle(&disabled).unwrap())
                .err(),
            Some(SendError::ReservedName)
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn guarded_broad_dispatch_refuses_changed_generation_snapshot_and_deletion_markers() {
        let dns = entries()
            .iter()
            .find(|entry| entry.id == "dns-resolved")
            .unwrap();
        let (commands, mut rx, stream) = connected(vec![]);
        let generation = *commands.stream_ready().borrow();
        let command = CuratedCommand::authorized_install(dns);
        // A new HELLO selected a new generation after the manager precheck.
        commands.rules.stage(None, vec![]);
        commands.on_reply(
            stream.id(),
            &snitchwatch_proto::protocol::NotificationReply {
                id: 0,
                code: snitchwatch_proto::protocol::NotificationReplyCode::Ok as i32,
                data: String::new(),
            },
        );
        assert_eq!(
            commands
                .send_curated_guarded(command, generation, false)
                .err(),
            Some(SendError::NoDaemon)
        );
        assert!(rx.try_recv().is_err());
        let generation = *commands.stream_ready().borrow();
        let mut edited = dns.rule();
        edited.operator.as_mut().unwrap().list[2].data = "5353".into();
        // A late daemon-confirmed edit between precheck and final admission.
        commands.rules.apply_confirmed(&Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![edited],
            ..Default::default()
        });
        assert_eq!(
            commands
                .send_curated_guarded(CuratedCommand::authorized_install(dns), generation, false)
                .err(),
            Some(SendError::ReservedName)
        );
        let mut disabled = dns.rule();
        disabled.enabled = false;
        assert_eq!(
            commands
                .send_curated_guarded(
                    CuratedCommand::authorized_toggle(&disabled).unwrap(),
                    generation,
                    true
                )
                .err(),
            Some(SendError::ReservedName)
        );
        assert!(rx.try_recv().is_err());
        // A refused delete removes memory but records a file that may return.
        commands.rules.apply_refused(&Notification {
            r#type: Action::DeleteRule as i32,
            rules: vec![dns.rule()],
            ..Default::default()
        });
        assert_eq!(
            commands
                .send_curated_guarded(CuratedCommand::authorized_install(dns), generation, false)
                .err(),
            Some(SendError::ReservedName)
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn guarded_tcp_reply_cannot_be_forged_by_a_new_current_peer() {
        let dns = entries()
            .iter()
            .find(|entry| entry.id == "dns-resolved")
            .unwrap();
        let sync = crate::cache::rules::RulesSync::new(tokio::sync::broadcast::channel(8).0);
        let commands = DaemonCommands::new(DaemonTransport::Tcp, sync.clone());
        let hello = snitchwatch_proto::protocol::NotificationReply {
            id: 0,
            code: snitchwatch_proto::protocol::NotificationReplyCode::Ok as i32,
            data: String::new(),
        };
        sync.stage(None, vec![]);
        let (first, mut first_rx) = commands.open_stream(None);
        commands.on_reply(first.id(), &hello);
        let generation = *commands.stream_ready().borrow();
        let _pending = commands
            .send_curated_guarded(CuratedCommand::authorized_install(dns), generation, false)
            .unwrap();
        let queued = first_rx.try_recv().unwrap();
        sync.stage(None, vec![]);
        let (other, mut other_rx) = commands.open_stream(None);
        commands.on_reply(other.id(), &hello);
        assert!(other_rx.try_recv().is_err());
        commands.on_reply(
            other.id(),
            &snitchwatch_proto::protocol::NotificationReply {
                id: queued.id,
                ..hello
            },
        );
        assert!(sync.cache().lock().unwrap().rules().unwrap().is_empty());
        assert!(commands
            .inner
            .lock()
            .unwrap()
            .waiters
            .contains_key(&queued.id));
    }

    /// The profile enforcer and the curated reconcile each send only under
    /// their own prefix: neither path can carry the other's rules.
    #[tokio::test]
    async fn the_curated_and_profile_paths_never_carry_each_others_rules() {
        use crate::daemon_commands::ProfileCommand;
        use crate::profiles::materializer::materialize_rule;
        use crate::profiles::store::ProfileRule;

        let profile_rule = materialize_rule(
            "home",
            &ProfileRule {
                id: "r1".into(),
                action: "deny".into(),
                operand: "dest.host".into(),
                data: "x.example".into(),
                operator: None,
            },
        )
        .unwrap();
        let curated_rule = flatpak().rule();
        let (commands, mut rx, _stream) =
            connected(vec![profile_rule.clone(), curated_rule.clone()]);

        // Curated path: no profile name, deleted or installed.
        assert!(CuratedCommand::delete(&profile_rule.name).is_none());
        let mut renamed = CuratedCommand::install(flatpak());
        renamed.notification.rules[0].name = profile_rule.name.clone();
        assert!(commands.send_curated(renamed).is_err());
        assert!(commands
            .send_curated_toggle(&profile_rule.name, false)
            .is_err());
        // Profile path: no curated name, deleted or installed.
        assert!(ProfileCommand::delete(&curated_rule.name).is_none());
        assert_eq!(
            commands
                .send_profile(ProfileCommand::install(curated_rule.clone()))
                .err(),
            Some(SendError::ReservedName)
        );
        assert!(!crate::profiles::materializer::made_by_bridge(
            &curated_rule
        ));
        assert!(rx.try_recv().is_err(), "nothing sent");
    }
}
