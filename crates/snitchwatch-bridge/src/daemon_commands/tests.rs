use super::*;
use crate::cache::rules::RulesCache;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Action, Operator, Rule};
use tokio::sync::broadcast;

/// Long enough for an in-process reply; a correlated reply resolves at
/// once, so only the expected-`Timeout` cases ever wait this out.
const SHORT: Duration = Duration::from_millis(100);
const LONG: Duration = Duration::from_secs(10);

struct Fixture {
    commands: DaemonCommands,
    rules: RulesSync,
    broadcasts: broadcast::Receiver<ServerMessage>,
}

fn fixture(transport: DaemonTransport) -> Fixture {
    let (tx, broadcasts) = broadcast::channel(256);
    let rules = RulesSync::new(tx);
    Fixture {
        commands: DaemonCommands::new(transport, rules.clone()),
        rules,
        broadcasts,
    }
}

fn commands(transport: DaemonTransport) -> DaemonCommands {
    fixture(transport).commands
}

fn command() -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![rule("899-firefox", true)],
        ..Default::default()
    }
}

fn rule(name: &str, enabled: bool) -> Rule {
    Rule {
        name: name.into(),
        enabled,
        action: "allow".into(),
        duration: "always".into(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn reply(id: u64, code: NotificationReplyCode, data: &str) -> NotificationReply {
    NotificationReply {
        id,
        code: code as i32,
        data: data.to_string(),
    }
}

fn hello() -> NotificationReply {
    reply(0, NotificationReplyCode::Ok, "")
}

fn ok(id: u64) -> NotificationReply {
    reply(id, NotificationReplyCode::Ok, "")
}

fn addr(port: u16) -> ConnKey {
    Some(SocketAddr::from(([127, 0, 0, 1], port)))
}

fn waiter_count(commands: &DaemonCommands) -> usize {
    lock(&commands.inner).waiters.len()
}

fn cached_names(rules: &RulesSync) -> Option<Vec<String>> {
    match &*rules.cache().lock().unwrap() {
        RulesCache::Unknown => None,
        RulesCache::Synced(cached) => Some(cached.keys().cloned().collect()),
    }
}

/// Names in the next `SetRules` broadcast, if there is one.
fn next_set_rules(rx: &mut broadcast::Receiver<ServerMessage>) -> Option<Vec<String>> {
    loop {
        match rx.try_recv() {
            Ok(ServerMessage::SetRules { rules }) => {
                return Some(
                    rules
                        .iter()
                        .map(|r| r["name"].as_str().unwrap().to_string())
                        .collect(),
                )
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn ids_start_at_one_and_increase() {
    let commands = commands(DaemonTransport::Tcp);
    let (_s1, mut rx1) = commands.open_stream(addr(1));
    let first = commands.send(command()).unwrap();
    let second = commands.send(command()).unwrap();
    assert_eq!(first.id(), 1, "id 0 is the daemon's HELLO");
    assert_eq!(second.id(), 2);
    assert_eq!(rx1.recv().await.unwrap().id, 1);
    assert_eq!(rx1.recv().await.unwrap().id, 2);
}

#[tokio::test]
async fn ok_reply_resolves_only_its_own_id() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());
    let first = commands.send(command()).unwrap();
    let second = commands.send(command()).unwrap();

    commands.on_reply(s1.id(), &ok(second.id()));

    assert_eq!(second.wait(LONG).await, Ok(()));
    assert_eq!(first.wait(SHORT).await, Err(CommandError::Timeout));
}

#[tokio::test]
async fn error_reply_surfaces_the_daemon_text() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());
    let pending = commands.send(command()).unwrap();

    commands.on_reply(
        s1.id(),
        &reply(pending.id(), NotificationReplyCode::Error, "no such rule"),
    );

    assert_eq!(
        pending.wait(LONG).await,
        Err(CommandError::Rejected("no such rule".into()))
    );
}

#[tokio::test]
async fn hello_makes_its_stream_current_and_bumps_stream_ready() {
    let commands = commands(DaemonTransport::Unix);
    let ready = commands.stream_ready();
    assert_eq!(*ready.borrow(), 0);
    let (s1, _rx1) = commands.open_stream(None);
    assert!(matches!(commands.send(command()), Err(SendError::NoDaemon)));

    assert!(commands.on_reply(s1.id(), &hello()));

    assert_eq!(*ready.borrow(), 1);
    assert!(commands.send(command()).is_ok(), "the stream is current");
    assert!(!commands.on_reply(s1.id(), &ok(42)));
}

/// Security-relevant: only the current stream's replies count.
#[tokio::test]
async fn reply_from_a_stream_that_is_no_longer_current_is_ignored() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    commands.on_reply(s2.id(), &hello());
    let pending = commands.send(command()).unwrap();

    // S2 (current) stays silent; S1's OK must not resolve the command.
    commands.on_reply(s1.id(), &ok(pending.id()));

    assert_eq!(pending.wait(SHORT).await, Err(CommandError::Timeout));
}

/// Unix hardening: a reply counts only from the stream the command went
/// to, even when another stream has become current since.
#[tokio::test]
async fn unix_reply_must_come_from_the_stream_the_command_went_to() {
    let commands = commands(DaemonTransport::Unix);
    let (s1, _rx1) = commands.open_stream(None);
    commands.on_reply(s1.id(), &hello());
    let pending = commands.send(command()).unwrap();
    let (s2, _rx2) = commands.open_stream(None);
    commands.on_reply(s2.id(), &hello());

    commands.on_reply(s2.id(), &ok(pending.id()));

    assert_eq!(pending.wait(SHORT).await, Err(CommandError::Timeout));
}

/// Security-relevant: a second local stream can't divert commands away
/// from the real daemon in legacy TCP mode.
#[tokio::test]
async fn tcp_fans_a_command_out_to_every_open_stream() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    let (s2, mut rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    commands.on_reply(s2.id(), &hello());

    let pending = commands.send(command()).unwrap();

    assert_eq!(rx1.try_recv().unwrap().id, pending.id());
    assert_eq!(rx2.try_recv().unwrap().id, pending.id());
}

#[tokio::test]
async fn unix_sends_a_command_only_to_the_current_stream() {
    let commands = commands(DaemonTransport::Unix);
    let (s1, mut rx1) = commands.open_stream(None);
    let (s2, mut rx2) = commands.open_stream(None);
    commands.on_reply(s1.id(), &hello());
    commands.on_reply(s2.id(), &hello());

    let pending = commands.send(command()).unwrap();

    assert_eq!(rx2.try_recv().unwrap().id, pending.id());
    assert!(
        rx1.try_recv().is_err(),
        "a non-current stream got a command"
    );
}

/// Security-relevant: a fake that sends a later HELLO and then closes
/// hands "current" back to the real daemon's still-open stream.
#[tokio::test]
async fn tcp_fallback_new_command_reaches_the_older_stream_and_its_reply_resolves() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    commands.on_reply(s2.id(), &hello());
    drop(s2);

    let pending = commands.send(command()).unwrap();
    assert_eq!(rx1.try_recv().unwrap().id, pending.id());
    commands.on_reply(s1.id(), &ok(pending.id()));

    assert_eq!(pending.wait(LONG).await, Ok(()));
}

/// The fallback picks the *newest* remaining HELLO, not the oldest.
#[tokio::test]
async fn fallback_after_the_current_stream_closes_is_the_newest_remaining_hello() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    let (s3, _rx3) = commands.open_stream(addr(3));
    for stream in [&s1, &s2, &s3] {
        commands.on_reply(stream.id(), &hello());
    }
    drop(s3);

    let from_oldest = commands.send(command()).unwrap();
    commands.on_reply(s1.id(), &ok(from_oldest.id()));
    assert_eq!(from_oldest.wait(SHORT).await, Err(CommandError::Timeout));

    let from_newest = commands.send(command()).unwrap();
    commands.on_reply(s2.id(), &ok(from_newest.id()));
    assert_eq!(from_newest.wait(LONG).await, Ok(()));
}

#[tokio::test]
async fn tcp_fallback_in_flight_command_resolves_from_the_older_stream() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    commands.on_reply(s2.id(), &hello());
    let pending = commands.send(command()).unwrap();
    assert_eq!(rx1.try_recv().unwrap().id, pending.id());

    drop(s2);
    commands.on_reply(s1.id(), &ok(pending.id()));

    assert_eq!(pending.wait(LONG).await, Ok(()));
}

#[tokio::test]
async fn tcp_last_stream_closing_fails_waiters_then_send_reports_no_daemon() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    let pending = commands.send(command()).unwrap();

    drop(s1);
    drop(s2);

    assert_eq!(pending.wait(LONG).await, Err(CommandError::StreamClosed));
    assert!(matches!(commands.send(command()), Err(SendError::NoDaemon)));
}

#[tokio::test]
async fn tcp_waiters_stay_pending_while_any_stream_is_open() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, _rx1) = commands.open_stream(addr(1));
    let (s2, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(s1.id(), &hello());
    let pending = commands.send(command()).unwrap();

    drop(s1);

    assert_eq!(pending.wait(SHORT).await, Err(CommandError::Timeout));
    drop(s2);
}

#[tokio::test]
async fn tcp_stream_without_hello_still_receives_commands() {
    let commands = commands(DaemonTransport::Tcp);
    let (_s1, mut rx1) = commands.open_stream(addr(1));

    let pending = commands
        .send(command())
        .expect("an open stream is a daemon");

    assert_eq!(rx1.try_recv().unwrap().id, pending.id());
}

#[tokio::test]
async fn unix_closing_the_current_stream_fails_its_waiters_and_send_reports_no_daemon() {
    let commands = commands(DaemonTransport::Unix);
    let (s1, _rx1) = commands.open_stream(None);
    commands.on_reply(s1.id(), &hello());
    let pending = commands.send(command()).unwrap();

    drop(s1);

    assert_eq!(pending.wait(LONG).await, Err(CommandError::StreamClosed));
    assert!(matches!(commands.send(command()), Err(SendError::NoDaemon)));
}

#[tokio::test]
async fn a_stream_that_never_reads_does_not_block_the_others() {
    let commands = commands(DaemonTransport::Tcp);
    let (_stalled, _never_read) = commands.open_stream(addr(1));
    let (s2, mut rx2) = commands.open_stream(addr(2));
    commands.on_reply(s2.id(), &hello());

    for _ in 0..STREAM_QUEUE_CAPACITY + 1 {
        let id = commands.send(command()).unwrap().id();
        assert_eq!(rx2.try_recv().unwrap().id, id);
    }
}

/// Defense in depth: `CHANGE_CONFIG` can repoint the root daemon's rules
/// path, server address and TLS files; `NONE` closes its stream.
#[tokio::test]
async fn only_rule_commands_ever_leave_the_bridge() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());
    for action in [
        Action::ChangeConfig,
        Action::None,
        Action::EnableInterception,
        Action::EnableRule,
        Action::TaskStart,
        Action::LogLevel,
        Action::Stop,
    ] {
        let notification = Notification {
            r#type: action as i32,
            data: r#"{"Rules":{"Path":"/tmp/x"}}"#.into(),
            ..command()
        };
        assert!(
            matches!(commands.send(notification), Err(SendError::NotAllowed)),
            "{action:?} was sent"
        );
    }
    assert!(rx1.try_recv().is_err());
    assert_eq!(waiter_count(&commands), 0);

    for allowed in [command(), toggle_off("a")] {
        assert!(commands.send(allowed).is_ok());
    }
}

/// L3: whatever built the command, an unsafe name never reaches the daemon.
#[tokio::test]
async fn a_rule_command_with_an_unsafe_name_never_leaves_the_bridge() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());
    for name in ["../default-config", "a/b", r"a\b", "", ".."] {
        for mut notification in [command(), toggle_off("x")] {
            notification.rules[0].name = name.to_string();
            assert!(
                matches!(commands.send(notification), Err(SendError::InvalidRuleName)),
                "{name:?} was sent"
            );
        }
    }
    assert!(rx1.try_recv().is_err());
    assert_eq!(waiter_count(&commands), 0);
}

#[tokio::test]
async fn a_command_no_stream_could_queue_is_an_error_without_a_waiter() {
    let commands = commands(DaemonTransport::Tcp);
    let (_stalled, _never_read) = commands.open_stream(addr(1));
    for _ in 0..STREAM_QUEUE_CAPACITY {
        drop(commands.send(command()).unwrap());
    }

    assert!(matches!(
        commands.send(command()),
        Err(SendError::NotQueued)
    ));
    assert_eq!(waiter_count(&commands), 0);
}

#[tokio::test]
async fn timed_out_and_dropped_waiters_are_forgotten() {
    let commands = commands(DaemonTransport::Tcp);
    let (_s1, _rx1) = commands.open_stream(addr(1));

    let timed_out = commands.send(command()).unwrap();
    assert_eq!(timed_out.wait(SHORT).await, Err(CommandError::Timeout));
    assert_eq!(waiter_count(&commands), 0);

    let dropped = commands.send(command()).unwrap();
    assert_eq!(waiter_count(&commands), 1);
    drop(dropped);
    assert_eq!(waiter_count(&commands), 0);
}

#[tokio::test]
async fn a_closed_stream_ends_its_outbound_receiver() {
    let commands = commands(DaemonTransport::Tcp);
    let (s1, mut rx1) = commands.open_stream(addr(1));
    drop(s1);
    assert!(rx1.recv().await.is_none());
}

// --- The rules list belongs to the stream that committed it (H1) -------

fn toggle_off(name: &str) -> Notification {
    Notification {
        r#type: Action::ChangeRule as i32,
        rules: vec![rule(name, false)],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_hello_commits_its_connections_snapshot_and_confirmed_changes_follow() {
    let Fixture {
        commands,
        rules,
        mut broadcasts,
    } = fixture(DaemonTransport::Tcp);
    rules.stage(addr(1), vec![rule("b", true), rule("a", true)]);
    let (s1, _rx1) = commands.open_stream(addr(1));

    commands.on_reply(s1.id(), &hello());
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["a", "b"]);

    let pending = commands.send(toggle_off("a")).unwrap();
    commands.on_reply(s1.id(), &ok(pending.id()));
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["a", "b"]);
    assert!(
        !rules.cache().lock().unwrap().snapshot_wire().unwrap()[0]["enabled"]
            .as_bool()
            .unwrap()
    );
}

/// Security-relevant (H1): a fake that subscribes, says HELLO and then
/// disconnects must not leave its forged list behind for the GUI to act
/// on under the real daemon's stream.
#[tokio::test]
async fn the_list_is_withdrawn_when_the_stream_that_committed_it_closes() {
    let Fixture {
        commands,
        rules,
        mut broadcasts,
    } = fixture(DaemonTransport::Tcp);
    rules.stage(addr(1), vec![rule("real", true)]);
    let (real, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(real.id(), &hello());
    rules.stage(addr(2), vec![rule("forged", true)]);
    let (fake, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(fake.id(), &hello());
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["real"]);
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["forged"]);

    drop(fake);

    assert_eq!(cached_names(&rules), None, "the forged list is gone");
    assert_eq!(
        next_set_rules(&mut broadcasts).unwrap(),
        Vec::<String>::new()
    );
    // A confirmed command no longer resurrects any list.
    let pending = commands.send(toggle_off("forged")).unwrap();
    commands.on_reply(real.id(), &ok(pending.id()));
    assert_eq!(pending.wait(LONG).await, Ok(()));
    assert_eq!(cached_names(&rules), None);
}

/// H1: another stream becoming current without a snapshot of its own
/// withdraws the list too.
#[tokio::test]
async fn the_list_is_withdrawn_when_its_stream_stops_being_current() {
    let Fixture {
        commands,
        rules,
        mut broadcasts,
    } = fixture(DaemonTransport::Tcp);
    rules.stage(addr(1), vec![rule("forged", true)]);
    let (fake, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(fake.id(), &hello());
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["forged"]);

    let (other, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(other.id(), &hello());

    assert_eq!(cached_names(&rules), None);
    assert_eq!(
        next_set_rules(&mut broadcasts).unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn closing_a_stream_that_did_not_commit_keeps_the_list() {
    let Fixture {
        commands,
        rules,
        mut broadcasts,
    } = fixture(DaemonTransport::Tcp);
    let (old, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(old.id(), &hello());
    rules.stage(addr(2), vec![rule("kept", true)]);
    let (new, _rx2) = commands.open_stream(addr(2));
    commands.on_reply(new.id(), &hello());
    assert_eq!(next_set_rules(&mut broadcasts).unwrap(), vec!["kept"]);

    drop(old);

    assert_eq!(cached_names(&rules), Some(vec!["kept".to_string()]));
    assert_eq!(next_set_rules(&mut broadcasts), None);
    drop(new);
}

#[tokio::test]
async fn confirmed_changes_apply_in_reply_order() {
    let Fixture {
        commands, rules, ..
    } = fixture(DaemonTransport::Tcp);
    rules.stage(addr(1), vec![rule("a", true)]);
    let (s1, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());
    let off = commands.send(toggle_off("a")).unwrap();
    let mut on = toggle_off("a");
    on.rules[0].enabled = true;
    let on = commands.send(on).unwrap();

    // The daemon answers the later command first.
    commands.on_reply(s1.id(), &ok(on.id()));
    commands.on_reply(s1.id(), &ok(off.id()));

    let cache = rules.cache();
    let enabled = cache.lock().unwrap().snapshot_wire().unwrap()[0]["enabled"]
        .as_bool()
        .unwrap();
    assert!(!enabled, "the last OK received wins");
}

#[tokio::test(start_paused = true)]
async fn a_late_ok_within_the_grace_period_still_reaches_the_cache() {
    let Fixture {
        commands, rules, ..
    } = fixture(DaemonTransport::Tcp);
    rules.stage(addr(1), vec![rule("a", true), rule("b", true)]);
    let (s1, _rx1) = commands.open_stream(addr(1));
    commands.on_reply(s1.id(), &hello());

    let late = commands.send(toggle_off("a")).unwrap();
    let late_id = late.id();
    assert_eq!(late.wait(SHORT).await, Err(CommandError::Timeout));
    commands.on_reply(s1.id(), &ok(late_id));

    let too_late = commands.send(toggle_off("b")).unwrap();
    let too_late_id = too_late.id();
    assert_eq!(too_late.wait(SHORT).await, Err(CommandError::Timeout));
    tokio::time::advance(LATE_REPLY_GRACE + Duration::from_secs(1)).await;
    commands.on_reply(s1.id(), &ok(too_late_id));

    let wire = rules.cache().lock().unwrap().snapshot_wire().unwrap();
    assert!(!wire[0]["enabled"].as_bool().unwrap(), "late OK applied");
    assert!(wire[1]["enabled"].as_bool().unwrap(), "too-late OK ignored");
}
