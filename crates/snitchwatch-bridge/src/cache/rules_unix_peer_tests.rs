//! Characterization for #391 (see `docs/superpowers/plans/2026-11-19-391-unix-peer-connkey.md`,
//! PR 1 task 1c): the system socket's redial cases under
//! `DaemonTransport::Unix` when the old and new connection carry DIFFERENT
//! keys (`PeerKey::Unix(1)` and `Unix(2)`), as per-connection ids would give
//! them. Nothing in production assigns those keys yet. The result decides
//! whether PR 2 gives the system socket per-connection ids or keeps its
//! shared `None` key. A test that fails is `#[ignore]`d with the reason and
//! asserts the behaviour a redialled daemon needs (the list survives); run
//! it with `--ignored` to see the current result.

use super::*;
use crate::daemon_commands::{ConnKey, DaemonCommands, DaemonTransport, PeerKey};
use snitchwatch_proto::protocol::{NotificationReply, NotificationReplyCode, Operator};
use tokio::sync::broadcast;

const OLD: ConnKey = Some(PeerKey::Unix(1));
const NEW: ConnKey = Some(PeerKey::Unix(2));

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

fn hello() -> NotificationReply {
    NotificationReply {
        code: NotificationReplyCode::Ok as i32,
        ..Default::default()
    }
}

fn system() -> (RulesSync, DaemonCommands) {
    let sync = RulesSync::new(broadcast::channel(64).0);
    let commands = DaemonCommands::new(DaemonTransport::Unix, sync.clone());
    (sync, commands)
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

/// Old connection subscribes and says HELLO; the redialled one subscribes
/// and says HELLO after it. The new stream gets its own connection's list.
#[test]
fn system_redial_new_hello_after_old_adopts_the_new_connections_list() {
    let (sync, commands) = system();
    sync.stage(OLD, vec![rule("old")]);
    let (old, _old_rx) = commands.open_stream(OLD);
    commands.on_reply(old.id(), &hello());
    sync.stage(NEW, vec![rule("new")]);
    let (new, _new_rx) = commands.open_stream(NEW);
    commands.on_reply(new.id(), &hello());
    assert_eq!(listed(&sync), vec!["new"]);
    drop(new);
    drop(old);
}

/// The old stream dies after the new one is current: the list stays.
#[test]
fn system_redial_old_closing_after_new_hello_keeps_the_list() {
    let (sync, commands) = system();
    sync.stage(OLD, vec![rule("old")]);
    let (old, _old_rx) = commands.open_stream(OLD);
    commands.on_reply(old.id(), &hello());
    sync.stage(NEW, vec![rule("new")]);
    let (new, _new_rx) = commands.open_stream(NEW);
    commands.on_reply(new.id(), &hello());
    drop(old);
    assert_eq!(listed(&sync), vec!["new"]);
    drop(new);
}

/// The other HELLO order: the new connection subscribes and opens its
/// stream, but the old stream's HELLO arrives after the new one's. The old
/// stream is then current; when it closes, the new stream must hold the
/// list. (With the shared key both adopted the same snapshot; with distinct
/// keys the new stream adopted another one.)
#[test]
fn system_redial_old_hello_after_new_hello_then_old_closing_keeps_a_list() {
    let (sync, commands) = system();
    sync.stage(OLD, vec![rule("old")]);
    let (old, _old_rx) = commands.open_stream(OLD);
    sync.stage(NEW, vec![rule("new")]);
    let (new, _new_rx) = commands.open_stream(NEW);
    commands.on_reply(new.id(), &hello());
    commands.on_reply(old.id(), &hello());
    drop(old);
    assert_eq!(listed(&sync), vec!["new"], "the new stream holds its list");
    drop(new);
}

/// Close-then-fallback: the newer stream closes, the older one falls back
/// to current and keeps the list it adopted.
#[test]
fn system_redial_closing_the_newer_stream_falls_back_with_a_list() {
    let (sync, commands) = system();
    sync.stage(OLD, vec![rule("old")]);
    let (old, _old_rx) = commands.open_stream(OLD);
    commands.on_reply(old.id(), &hello());
    sync.stage(NEW, vec![rule("new")]);
    let (new, _new_rx) = commands.open_stream(NEW);
    commands.on_reply(new.id(), &hello());
    drop(new);
    assert_eq!(listed(&sync), vec!["old"], "the fallback holds its list");
    drop(old);
}

/// The Subscribe/Notifications split of plan section 3: Subscribe on
/// connection 1, the Notifications stream and its HELLO on connection 2.
/// FAILS with distinct keys (the HELLO finds no snapshot for connection 2 and
/// the list is withdrawn), so it is `#[ignore]`d: it is the case that argues
/// for keeping the system socket's shared key (decision for PR 2).
#[test]
#[ignore = "#391: Subscribe on conn 1, HELLO on conn 2 => no snapshot for conn 2, the list is withdrawn (empty instead of [\"a\"]); the system socket keeps the shared None key because of this"]
fn known_failing_system_subscribe_on_one_connection_and_hello_on_another_gets_the_list() {
    let (sync, commands) = system();
    sync.stage(OLD, vec![rule("a")]);
    let (stream, _rx) = commands.open_stream(NEW);
    commands.on_reply(stream.id(), &hello());
    assert_eq!(listed(&sync), vec!["a"]);
    drop(stream);
}

/// The same split under the shared key, which is what the system socket does
/// today (every peer is `None`): the list comes through. A change that gives
/// system-socket peers distinct keys breaks this test.
#[test]
fn system_subscribe_on_one_connection_and_hello_on_another_gets_the_list_under_the_shared_key() {
    let (sync, commands) = system();
    sync.stage(None, vec![rule("a")]);
    let (stream, _rx) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello());
    assert_eq!(listed(&sync), vec!["a"]);
    drop(stream);
}
