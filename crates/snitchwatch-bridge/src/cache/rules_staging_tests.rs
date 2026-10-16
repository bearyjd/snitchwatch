//! Staged snapshots (#115): a `Subscribe` that finishes after a newer one
//! doesn't replace it, and adopting the same snapshot again keeps the
//! changes confirmed since it was first adopted.

use super::*;
use crate::daemon_commands::{DaemonCommands, DaemonTransport};
use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode, Operator};
use tokio::sync::broadcast;

fn rule(name: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
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

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![rule(name)],
        ..Default::default()
    }
}

fn listed(sync: &RulesSync) -> Vec<String> {
    let cache = lock(&sync.cache);
    let mut names: Vec<String> = cache
        .rules()
        .map(|rules| rules.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn hello() -> NotificationReply {
    NotificationReply {
        code: NotificationReplyCode::Ok as i32,
        ..Default::default()
    }
}

fn unix(rules: Vec<Rule>) -> (RulesSync, DaemonCommands) {
    let sync = RulesSync::new(broadcast::channel(64).0);
    let commands = DaemonCommands::new(DaemonTransport::Unix, sync.clone());
    sync.stage(None, rules);
    (sync, commands)
}

/// Subscribe A waits on account names while B arrives and stages; A's older
/// snapshot must not replace B's on the Unix socket's shared key.
#[test]
fn an_older_subscribe_staged_late_does_not_replace_a_newer_one() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    let a = pending.receive();
    let b = pending.receive();
    pending.stage_received(None, vec![rule("b")], now, b);
    pending.stage_received(None, vec![rule("a")], now, a);
    let adopted = pending.adopt_fresh(&None, 1, now).unwrap();
    assert_eq!(adopted.rules[0].name, "b");
    assert_eq!(adopted.sequence, b);
}

#[test]
fn a_newer_subscribe_replaces_an_older_one_and_sequences_are_unique() {
    let now = Instant::now();
    let mut pending = PendingSnapshots::default();
    let a = pending.receive();
    pending.stage_received(None, vec![rule("a")], now, a);
    pending.stage(None, vec![rule("b")], now);
    let adopted = pending.adopt_fresh(&None, 1, now).unwrap();
    assert_eq!(adopted.rules[0].name, "b");
    assert!(adopted.sequence > a);
}

/// A stale entry of the key, even a newer one, never blocks a fresh stage.
#[test]
fn a_stale_newer_entry_does_not_block_an_older_fresh_one() {
    let then = Instant::now();
    let mut pending = PendingSnapshots::default();
    let a = pending.receive();
    let b = pending.receive();
    pending.stage_received(None, vec![rule("b")], then, b);
    let later = then + Duration::from_secs(31);
    pending.stage_received(None, vec![rule("a")], later, a);
    assert_eq!(
        pending.adopt_fresh(&None, 1, later).unwrap().rules[0].name,
        "a"
    );
}

/// The receipt's revision and arrival order both come from before any
/// await in `Subscribe`.
#[test]
fn the_sync_stages_by_receipt() {
    let sync = RulesSync::new(broadcast::channel(8).0);
    let a = sync.receive();
    let b = sync.receive();
    sync.stage_received(None, vec![rule("b")], b);
    sync.stage_received(None, vec![rule("a")], a);
    let pending = lock(&sync.pending);
    assert_eq!(pending.entries.len(), 1);
    assert_eq!(pending.entries[0].snapshot.rules[0].name, "b");
}

/// PR #106 OQ1's cost, fixed (#115): the newer stream's HELLO adopts the
/// same snapshot again, and a change confirmed on the older stream between
/// the two HELLOs stays on the list.
#[test]
fn a_second_hello_keeps_changes_confirmed_since_the_first() {
    let (sync, commands) = unix(vec![rule("a"), rule("b")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello());
    sync.apply_confirmed(&change(rule("c")));
    sync.apply_confirmed(&delete("a"));
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    assert_eq!(listed(&sync), vec!["b", "c"]);
    drop(newer);
    drop(older);
}

/// N1's fallback (`readopt`) keeps them too, including a remembered prompt
/// answer.
#[test]
fn a_readoption_keeps_changes_confirmed_since_the_first_adoption() {
    let (sync, commands) = unix(vec![rule("a")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello());
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    sync.apply_confirmed(&change(rule("c")));
    sync.upsert(rule("prompt"));
    drop(newer);
    assert_eq!(listed(&sync), vec!["a", "c", "prompt"]);
    drop(older);
}

/// A refused delete took the rule out of the daemon's memory (tower r12):
/// adopting the snapshot again doesn't bring it back.
#[test]
fn a_second_hello_keeps_a_refused_delete() {
    let (sync, commands) = unix(vec![rule("a"), rule("b")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello());
    sync.apply_refused(&delete("a"));
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    assert_eq!(listed(&sync), vec!["b"]);
    drop(newer);
    drop(older);
}

/// A new snapshot is the daemon's word: nothing from the old one's journal
/// is replayed onto it.
#[test]
fn a_new_snapshot_replays_nothing() {
    let (sync, commands) = unix(vec![rule("a")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello());
    sync.apply_confirmed(&change(rule("c")));
    sync.stage(None, vec![rule("x")]);
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    assert_eq!(listed(&sync), vec!["x"]);
    drop(newer);
    drop(older);
}

/// Past its cap the journal can't be trusted: a readoption withdraws the
/// list, as before the journal.
#[test]
fn an_overflowed_journal_withdraws_instead_of_readopting() {
    let (sync, commands) = unix(vec![rule("a")]);
    let (older, _older_rx) = commands.open_stream(None);
    commands.on_reply(older.id(), &hello());
    let (newer, _newer_rx) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    for i in 0..=MAX_JOURNAL {
        sync.apply_confirmed(&change(rule(&format!("r{i}"))));
    }
    drop(newer);
    assert!(lock(&sync.cache).is_unknown());
    drop(older);
}
