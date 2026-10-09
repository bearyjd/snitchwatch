//! [`DaemonCommands::in_flight`]: the commands whose answer may still change
//! the rules cache (the rules-count hint pauses while any exist).

use super::*;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::{Operator, Rule};
use tokio::sync::broadcast;

fn commands() -> DaemonCommands {
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    DaemonCommands::new(DaemonTransport::Tcp, RulesSync::new(tx))
}

fn command() -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: "899-firefox".into(),
            action: "allow".into(),
            duration: "always".into(),
            operator: Some(Operator {
                r#type: "simple".into(),
                operand: "dest.host".into(),
                data: "example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn reply(id: u64) -> NotificationReply {
    NotificationReply {
        id,
        code: NotificationReplyCode::Ok as i32,
        data: String::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn a_command_is_in_flight_until_it_is_answered() {
    let commands = commands();
    assert_eq!(commands.in_flight(), 0);
    let (stream, _rx) = commands.open_stream(None);
    commands.on_reply(stream.id(), &reply(0));
    let first = commands.send(command()).unwrap();
    let second = commands.send(command()).unwrap();
    assert_eq!(commands.in_flight(), 2);
    commands.on_reply(stream.id(), &reply(first.id()));
    assert_eq!(commands.in_flight(), 1);
    assert_eq!(first.wait(Duration::from_secs(1)).await, Ok(()));
    // A caller that drops its handle no longer waits.
    drop(second);
    assert_eq!(commands.in_flight(), 0);
}

/// An `OK` that comes after the timeout is still applied to the cache, for
/// `LATE_REPLY_GRACE`: until then the daemon may have changed what the
/// bridge doesn't know yet.
#[tokio::test(start_paused = true)]
async fn a_command_that_timed_out_stays_in_flight_for_the_late_reply_grace() {
    let commands = commands();
    let (stream, _rx) = commands.open_stream(None);
    commands.on_reply(stream.id(), &reply(0));
    let pending = commands.send(command()).unwrap();
    let id = pending.id();
    assert_eq!(
        pending.wait(Duration::from_secs(5)).await,
        Err(CommandError::Timeout)
    );
    assert_eq!(commands.in_flight(), 1);
    tokio::time::advance(LATE_REPLY_GRACE - Duration::from_secs(1)).await;
    assert_eq!(commands.in_flight(), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(commands.in_flight(), 0, "the grace is over");

    // Answered late, within the grace: no longer in flight.
    let pending = commands.send(command()).unwrap();
    let id2 = pending.id();
    assert_ne!(id, id2);
    assert_eq!(
        pending.wait(Duration::from_secs(5)).await,
        Err(CommandError::Timeout)
    );
    assert_eq!(commands.in_flight(), 1);
    commands.on_reply(stream.id(), &reply(id2));
    assert_eq!(commands.in_flight(), 0);
}
