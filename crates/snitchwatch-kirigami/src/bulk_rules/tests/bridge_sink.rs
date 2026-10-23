//! `BridgeSink` over real channels (no WebSocket): the tests play the
//! bridge. Each test owns a multi-thread runtime and runs OFF it, the way
//! the sequencer's worker thread does.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use tokio::runtime::Runtime;

use crate::bridge_runtime::test_link::TestLink;
use crate::bridge_runtime::ReceivedServerMessage;
use crate::bulk_rules::{BridgeSink, Incoming, RuleSink, SendError, SessionLost};

const WAIT: Duration = Duration::from_millis(20);

fn setup(queue: usize, feed: usize) -> (Runtime, TestLink, BridgeSink) {
    let runtime = Runtime::new().expect("runtime");
    let link = TestLink::new(runtime.handle().clone(), queue, feed);
    let sink = BridgeSink::new(link.handles.clone(), Arc::new(AtomicBool::new(false)));
    (runtime, link, sink)
}

fn from_session(link: &TestLink, connection_id: u64, message: ServerMessage) {
    link.feed
        .send(ReceivedServerMessage {
            connection_id,
            message,
        })
        .expect("a subscriber");
}

#[test]
fn messages_arrive_and_a_quiet_feed_is_idle() {
    let (_rt, link, mut sink) = setup(4, 8);
    assert_eq!(sink.next_message(WAIT), Ok(Incoming::Idle));
    from_session(&link, 1, ServerMessage::ClearConnectionRows);
    assert_eq!(
        sink.next_message(WAIT),
        Ok(Incoming::Message(ServerMessage::ClearConnectionRows))
    );
}

#[test]
fn the_sink_pins_to_the_first_session_that_answers() {
    let (_rt, link, mut sink) = setup(4, 8);
    from_session(&link, 1, ServerMessage::ClearConnectionRows);
    assert!(matches!(sink.next_message(WAIT), Ok(Incoming::Message(_))));
    from_session(&link, 2, ServerMessage::ClearConnectionRows);
    assert_eq!(sink.next_message(WAIT), Err(SessionLost));
}

#[test]
fn sends_go_out_on_the_pinned_session_only() {
    let (_rt, mut link, mut sink) = setup(4, 8);
    assert_eq!(sink.request_snapshot(), Ok(()));
    let first = link.queued.blocking_recv().expect("queued");
    assert!(matches!(first, ClientMessage::RequestSnapshot));
    from_session(&link, 1, ServerMessage::ClearConnectionRows);
    sink.next_message(WAIT).expect("pinned to 1");
    link.set_session(true, 2); // the bridge reconnected
    assert_eq!(sink.request_snapshot(), Err(SendError::NotConnected));
}

#[test]
fn a_full_queue_and_no_connection_are_told_apart() {
    let (_rt, link, mut sink) = setup(1, 8);
    // The test's forwarder drains the queue, so fill it faster than that.
    let mut results = Vec::new();
    for _ in 0..200 {
        results.push(sink.request_snapshot());
    }
    assert!(results.contains(&Err(SendError::Full)));
    link.set_session(false, 1);
    assert_eq!(sink.request_snapshot(), Err(SendError::NotConnected));
}

#[test]
fn add_rule_carries_the_request_id() {
    let (_rt, mut link, mut sink) = setup(4, 8);
    let payload = super::fake::payload_with("a", "/usr/bin/a");
    assert_eq!(sink.send_add("bulk-1", &payload), Ok(()));
    match link.queued.blocking_recv().expect("queued") {
        ClientMessage::AddRule {
            request_id, rule, ..
        } => {
            assert_eq!(request_id.as_deref(), Some("bulk-1"));
            assert_eq!(rule["name"], "a");
        }
        _ => panic!("not an AddRule"),
    }
    let id = sink.new_request_id();
    assert!(
        snitchwatch_bridge::ws_messages::valid_request_id(&id),
        "{id}"
    );
}

#[test]
fn a_lag_is_reported_as_one() {
    let (_rt, link, mut sink) = setup(4, 2);
    for _ in 0..6 {
        from_session(&link, 1, ServerMessage::ClearConnectionRows);
    }
    assert_eq!(sink.next_message(WAIT), Ok(Incoming::Lagged));
    assert!(matches!(sink.next_message(WAIT), Ok(Incoming::Message(_))));
}

#[test]
fn cancel_is_read_from_the_flag() {
    let runtime = Runtime::new().expect("runtime");
    let link = TestLink::new(runtime.handle().clone(), 4, 8);
    let flag = Arc::new(AtomicBool::new(false));
    let sink = BridgeSink::new(link.handles.clone(), flag.clone());
    assert!(!sink.cancelled());
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(sink.cancelled());
}

#[test]
fn waiting_on_a_runtime_thread_is_refused_not_a_panic() {
    let (rt, _link, mut sink) = setup(4, 8);
    let result = rt.block_on(async { sink.next_message(WAIT) });
    assert_eq!(result, Err(SessionLost));
}
