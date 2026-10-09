//! A model of opensnitchd v1.8.0's rule loader
//! (`vendor/opensnitch/daemon/rule/loader.go`), for tests that depend on
//! what a refused command leaves behind (tower r12; plan
//! `docs/superpowers/plans/2026-10-08-refused-delete-honesty.md`).
//!
//! In the loader's real order:
//! - `DELETE_RULE` (`Delete`): a name not in memory is `OK`, and nothing
//!   else happens. Otherwise the rule leaves memory **first**; then an
//!   `always` rule's file is removed, and only that can fail: a stuck file
//!   (`chattr +i`) answers `ERROR` with the rule already gone from memory.
//! - `CHANGE_RULE` (`Replace` → `replaceUserRule` → `Save`):
//!   1. no operator: `ERROR`, nothing changed (`Deserialize`);
//!   2. an `always` rule in memory changed to a temporary one loses its
//!      file first (`deleteOldRuleFromDisk`; a stuck file stays, as the
//!      daemon only logs that error);
//!   3. an enabled rule that doesn't compile ([`validate_rule_shape`])
//!      answers `ERROR`, the old rule kept in memory;
//!   4. otherwise the rule replaces the old one in memory, and an `always`
//!      rule's file is written, unless it is stuck: `ERROR`, with the
//!      rule already in memory and the old file left as it was.
//! - [`LoaderModel::restart`]: memory is what the files load as, in the
//!   daemon's reported shape ([`as_daemon_reports`]).
//! - [`LoaderModel::add_prompt_answer`] (`Loader.Add`): a name already in
//!   memory is stored as `<name>-2`, `-3`, ... (`setUniqueName`).
//! - An enabled temporary rule whose duration Go can't parse is stored,
//!   then refused (`scheduleTemporaryRule`).
//!
//! Not modelled: live reload (the watcher's reaction to a removed file),
//! temporary rules' timers, and multi-rule commands (the bridge sends one
//! rule per command).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Rule,
};
use tokio::sync::mpsc;

use crate::round_trip::as_daemon_reports;
use crate::{validate_duration, validate_rule_shape};

/// What a stuck file's removal or write answers.
pub const NOT_PERMITTED: &str = "operation not permitted";

#[derive(Debug, Default, Clone)]
pub struct LoaderModel {
    /// Rules that apply, by name.
    pub memory: BTreeMap<String, Rule>,
    /// Rule files on disk, by name, as last written.
    pub files: BTreeMap<String, Rule>,
    /// Files that can be neither removed nor written (`chattr +i`).
    pub stuck: BTreeSet<String>,
}

pub type SharedLoader = Arc<Mutex<LoaderModel>>;

impl LoaderModel {
    /// A daemon that loaded `rules` from disk (`always` ones have files).
    pub fn with_rules(rules: &[Rule]) -> Self {
        let mut model = Self::default();
        for rule in rules {
            if rule.duration == "always" {
                model.files.insert(rule.name.clone(), rule.clone());
            }
            model.memory.insert(rule.name.clone(), rule.clone());
        }
        model
    }

    pub fn shared(self) -> SharedLoader {
        Arc::new(Mutex::new(self))
    }

    /// The daemon's answer to one command: `Ok(())` or its `ERROR` text.
    pub fn apply(&mut self, n: &Notification) -> Result<(), String> {
        let Some(rule) = n.rules.first() else {
            return Ok(());
        };
        if n.r#type == Action::DeleteRule as i32 {
            return self.delete(&rule.name);
        }
        if n.r#type == Action::ChangeRule as i32 {
            return self.change(rule.clone());
        }
        Ok(())
    }

    fn delete(&mut self, name: &str) -> Result<(), String> {
        let Some(old) = self.memory.remove(name) else {
            return Ok(());
        };
        if old.duration != "always" {
            return Ok(());
        }
        if self.stuck.contains(name) {
            return Err(format!("remove {name}.json: {NOT_PERMITTED}"));
        }
        // `os.Remove` of a missing file fails too.
        self.files
            .remove(name)
            .map(drop)
            .ok_or_else(|| format!("remove {name}.json: no such file or directory"))
    }

    fn change(&mut self, rule: Rule) -> Result<(), String> {
        if rule.operator.is_none() {
            return Err("Invalid rule, invalid operator".into());
        }
        let loses_file = self
            .memory
            .get(&rule.name)
            .is_some_and(|old| old.duration == "always" && rule.duration != "always");
        if loses_file && !self.stuck.contains(&rule.name) {
            self.files.remove(&rule.name);
        }
        if rule.enabled {
            // `Compile` (before the rule is stored); the duration is read
            // later, by `scheduleTemporaryRule`.
            let mut compiled = rule.clone();
            compiled.duration = "always".into();
            validate_rule_shape(&compiled).map_err(|e| format!("(2) error compiling rule: {e}"))?;
        }
        self.memory.insert(rule.name.clone(), rule.clone());
        // `scheduleTemporaryRule` (`time.ParseDuration`) fails after the
        // rule is stored: `ERROR`, and the rule is in memory all the same.
        if rule.enabled
            && !matches!(rule.duration.as_str(), "once" | "until restart" | "always")
            && validate_duration(&rule.duration).is_err()
        {
            return Err(format!("time: invalid duration \"{}\"", rule.duration));
        }
        if rule.duration != "always" {
            return Ok(());
        }
        if self.stuck.contains(&rule.name) {
            return Err(format!(
                "Error while saving rule {0} to {0}.json: {NOT_PERMITTED}",
                rule.name
            ));
        }
        self.files.insert(rule.name.clone(), rule);
        Ok(())
    }

    /// The daemon stores a prompt answer: `Loader.Add` (`main.go` after
    /// `Ask`) → `addUserRule` → `setUniqueName` → `replaceUserRule`, then
    /// `Save` for an `always` rule. A `once` answer is not stored. A name
    /// already in memory, a disabled rule's included, gets `-2`, `-3`, ...
    /// so the daemon then holds one rule more than a list that replaced the
    /// name. Returns the name it stored the rule under, and `Err` as
    /// `change` does (a rule that doesn't compile is not stored).
    pub fn add_prompt_answer(&mut self, mut rule: Rule) -> Result<Option<String>, String> {
        if rule.duration == "once" {
            return Ok(None);
        }
        let base = rule.name.clone();
        let mut n = 1;
        while self.memory.contains_key(&rule.name) {
            n += 1;
            rule.name = format!("{base}-{n}");
        }
        let stored = rule.name.clone();
        self.change(rule).map(|()| Some(stored))
    }

    /// The daemon restarts: memory is what its files load as.
    pub fn restart(&mut self) {
        self.memory = self
            .files
            .iter()
            .map(|(name, rule)| (name.clone(), as_daemon_reports(rule)))
            .collect();
    }

    /// `Statistics.rules` in a `Ping`: `Loader.NumRules()`, which is
    /// `len(l.rules)`: every rule in memory, disabled and temporary ones
    /// too, and none that left it (a refused delete's file does not count).
    pub fn num_rules(&self) -> u64 {
        self.memory.len() as u64
    }

    /// `Subscribe`'s `ClientConfig.rules`: everything in memory.
    pub fn snapshot(&self) -> Vec<Rule> {
        self.memory.values().cloned().collect()
    }
}

/// Answer every command arriving on `inbound` (from
/// [`crate::MockOpensnitchd::open_notifications`]) from `model`, through
/// `replies`, and forward each command to the returned receiver once it is
/// applied (before its answer is sent).
///
/// Dropping the returned receiver stands for this daemon process exiting:
/// the next command is neither applied nor answered, and the stream closes.
/// (On TCP the bridge fans a command out to every open stream, so a
/// lingering one would otherwise apply it to the shared model too.)
pub fn spawn_loader_responder(
    model: SharedLoader,
    replies: mpsc::Sender<NotificationReply>,
    mut inbound: mpsc::Receiver<Notification>,
) -> mpsc::Receiver<Notification> {
    let (seen_tx, seen_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(n) = inbound.recv().await {
            if seen_tx.is_closed() {
                return;
            }
            let outcome = model.lock().unwrap_or_else(|e| e.into_inner()).apply(&n);
            let reply = NotificationReply {
                id: n.id,
                code: match outcome {
                    Ok(()) => NotificationReplyCode::Ok as i32,
                    Err(_) => NotificationReplyCode::Error as i32,
                },
                data: outcome.err().unwrap_or_default(),
            };
            let _ = seen_tx.send(n).await;
            if replies.send(reply).await.is_err() {
                return;
            }
        }
    });
    seen_rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_proto::protocol::Operator;

    fn rule(name: &str, enabled: bool, duration: &str) -> Rule {
        Rule {
            name: name.into(),
            enabled,
            action: "allow".into(),
            duration: duration.into(),
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "dest.host".into(),
                data: "example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn delete(name: &str) -> Notification {
        Notification {
            r#type: Action::DeleteRule as i32,
            rules: vec![Rule {
                name: name.into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn change(rule: Rule) -> Notification {
        Notification {
            r#type: Action::ChangeRule as i32,
            rules: vec![rule],
            ..Default::default()
        }
    }

    /// The tower r12 sequence, as the daemon sees it.
    #[test]
    fn a_stuck_file_is_refused_after_the_rule_left_memory_and_a_second_delete_is_ok() {
        let mut model = LoaderModel::with_rules(&[rule("a", true, "always")]);
        model.stuck.insert("a".into());
        let refused = model.apply(&delete("a")).unwrap_err();
        assert!(refused.contains(NOT_PERMITTED), "{refused}");
        assert!(!model.memory.contains_key("a"), "gone from memory first");
        assert!(model.files.contains_key("a"), "its file stays");

        model.stuck.clear();
        assert_eq!(model.apply(&delete("a")), Ok(()), "not in memory: OK");
        assert!(model.files.contains_key("a"), "and the file is untouched");

        model.restart();
        assert!(model.memory.contains_key("a"), "it loads again");
        assert_eq!(model.apply(&delete("a")), Ok(()));
        assert!(model.files.is_empty() && model.memory.is_empty());
    }

    #[test]
    fn num_rules_counts_every_rule_in_memory_as_the_daemon_does() {
        let mut model = LoaderModel::with_rules(&[
            rule("on", true, "always"),
            rule("off", false, "always"),
            rule("timed", true, "5m"),
        ]);
        assert_eq!(model.num_rules(), 3, "disabled and temporary count");
        model.stuck.insert("on".into());
        assert!(model.apply(&delete("on")).is_err());
        assert_eq!(model.num_rules(), 2, "a refused delete left memory first");
        assert_eq!(model.snapshot().len() as u64, model.num_rules());
    }

    #[test]
    fn a_duration_go_cannot_parse_is_refused_after_the_rule_is_stored() {
        let mut model = LoaderModel::default();
        let refused = model
            .apply(&change(rule("t", true, "5 minutes")))
            .unwrap_err();
        assert!(refused.contains("invalid duration"), "{refused}");
        assert!(model.memory.contains_key("t"), "stored before the error");
        assert_eq!(model.num_rules(), 1);
        // A disabled rule is never scheduled.
        assert_eq!(model.apply(&change(rule("d", false, "5 minutes"))), Ok(()));
        // The duration Go reads and the bridge doesn't.
        assert_eq!(model.apply(&change(rule("ok", true, "1.5h"))), Ok(()));
    }

    #[test]
    fn a_prompt_answer_under_a_name_in_memory_is_stored_under_a_new_one() {
        let mut model = LoaderModel::with_rules(&[rule("seen", false, "always")]);
        assert_eq!(
            model.add_prompt_answer(rule("seen", true, "always")),
            Ok(Some("seen-2".to_string()))
        );
        assert_eq!(
            model.add_prompt_answer(rule("seen", true, "5m")),
            Ok(Some("seen-3".to_string()))
        );
        assert_eq!(model.num_rules(), 3, "the disabled original stays");
        assert!(!model.memory["seen"].enabled);
        assert!(
            model.files.contains_key("seen-2"),
            "an always rule is saved"
        );
        assert!(!model.files.contains_key("seen-3"), "a timed one is not");
        // Under a new name it is stored as it is, and a `once` is not stored.
        assert_eq!(
            model.add_prompt_answer(rule("fresh", true, "always")),
            Ok(Some("fresh".to_string()))
        );
        assert_eq!(
            model.add_prompt_answer(rule("fresh", true, "once")),
            Ok(None)
        );
        assert_eq!(model.num_rules(), 4);
    }

    #[test]
    fn a_temporary_rule_is_deleted_without_touching_any_file() {
        let mut model = LoaderModel::with_rules(&[rule("t", true, "5m")]);
        model.stuck.insert("t".into());
        assert_eq!(model.apply(&delete("t")), Ok(()));
        assert!(model.memory.is_empty());
    }

    #[test]
    fn a_stuck_file_refuses_a_change_after_memory_took_it() {
        let mut model = LoaderModel::with_rules(&[rule("a", true, "always")]);
        model.stuck.insert("a".into());
        assert!(model.apply(&change(rule("a", false, "always"))).is_err());
        assert!(!model.memory["a"].enabled, "memory took the change");
        assert!(model.files["a"].enabled, "the file is the old one");
    }

    #[test]
    fn an_enabled_rule_that_does_not_compile_leaves_the_old_one() {
        let mut model = LoaderModel::with_rules(&[rule("a", false, "always")]);
        let mut bad = rule("a", true, "always");
        bad.operator.as_mut().unwrap().r#type = "regexp".into();
        bad.operator.as_mut().unwrap().data = "(".into();
        assert!(model.apply(&change(bad)).unwrap_err().contains("compiling"));
        assert!(!model.memory["a"].enabled);
    }

    #[test]
    fn an_always_rule_made_temporary_loses_its_file_first() {
        let mut model = LoaderModel::with_rules(&[rule("a", true, "always")]);
        assert_eq!(model.apply(&change(rule("a", true, "5m"))), Ok(()));
        assert!(model.files.is_empty());
        assert_eq!(model.memory["a"].duration, "5m");
    }
}
