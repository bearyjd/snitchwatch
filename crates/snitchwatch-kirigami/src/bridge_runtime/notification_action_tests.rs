//! Prompt-slot plan Part B, through the runtime's session state: a desktop
//! notification's "Allow once" and "Deny" do what the inline buttons do,
//! for the row's own session, and do nothing for a row that no longer waits
//! there. (Kept out of `tests.rs`, which is near the 800-line cap.)

use super::*;
use crate::inline_deny::InlineDeny;
use crate::notification_actions::{act, still_waiting, ActionOutcome, NoticeAction};
use snitchwatch_bridge::ws_messages::{
    ConnectionRow, VerdictAction, VerdictDuration, VerdictScope,
};

fn handles_and_queue() -> (
    BridgeHandles,
    Arc<Mutex<ConnectionState>>,
    mpsc::Receiver<QueuedClientMessage>,
) {
    let (broadcast_tx, _) = broadcast::channel(1);
    let (inbound_tx, inbound_rx) = mpsc::channel(8);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };
    (handles, connection, inbound_rx)
}

fn row(id: &str, process_path: Option<&str>) -> ConnectionRow {
    ConnectionRow {
        id: id.into(),
        process: "curl".into(),
        process_path: process_path.map(str::to_owned),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: None,
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
    }
}

fn insert(connection: &Mutex<ConnectionState>, session: u64, rows: Vec<ConnectionRow>) {
    pending_rows::observe(
        connection,
        session,
        &ServerMessage::InsertConnectionRows { rows },
    );
}

/// The next queued verdict: (session, wire row id, verdict, duration).
fn queued(
    inbound_rx: &mut mpsc::Receiver<QueuedClientMessage>,
) -> (u64, String, VerdictAction, VerdictDuration) {
    let queued = inbound_rx.try_recv().expect("a verdict was queued");
    match queued.message {
        ClientMessage::SetVerdict {
            row_id,
            verdict,
            scope,
            duration,
            remember,
        } => {
            assert_eq!(scope, VerdictScope::ThisHost);
            let duration =
                snitchwatch_bridge::ws_messages::effective_verdict_duration(duration, remember);
            (queued.connection_id, row_id, verdict, duration)
        }
        other => panic!("expected SetVerdict, got {other:?}"),
    }
}

#[tokio::test]
async fn allow_once_is_once_and_goes_to_the_rows_session() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    insert(
        &connection,
        session,
        vec![row("ask-1", Some("/usr/bin/curl"))],
    );

    assert_eq!(
        act(&handles, session, "ask-1", NoticeAction::AllowOnce),
        ActionOutcome::Sent
    );
    assert_eq!(
        queued(&mut inbound_rx),
        (
            session,
            "ask-1".into(),
            VerdictAction::Allow,
            VerdictDuration::Once
        )
    );
}

#[tokio::test]
async fn deny_is_remembered_only_for_a_known_program_on_a_capable_bridge() {
    let cases = [
        (
            true,
            Some("/usr/bin/curl"),
            VerdictDuration::UntilRestart,
            ActionOutcome::Sent,
        ),
        (
            false,
            Some("/usr/bin/curl"),
            VerdictDuration::Once,
            ActionOutcome::DeniedOnce(InlineDeny::BridgeTooOld),
        ),
        (
            true,
            Some("Kernel connection"),
            VerdictDuration::Once,
            ActionOutcome::DeniedOnce(InlineDeny::ProgramUnknown),
        ),
        (
            true,
            None,
            VerdictDuration::Once,
            ActionOutcome::DeniedOnce(InlineDeny::ProgramUnknown),
        ),
    ];
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    for (app_bound_rules, path, duration, outcome) in cases {
        disconnect_and_discard(&connection, &mut inbound_rx);
        let session = mark_connected(&connection, app_bound_rules);
        insert(&connection, session, vec![row("ask-1", path)]);
        assert_eq!(
            act(&handles, session, "ask-1", NoticeAction::Deny),
            outcome,
            "{app_bound_rules} {path:?}"
        );
        assert_eq!(
            queued(&mut inbound_rx),
            (session, "ask-1".into(), VerdictAction::Deny, duration),
            "{app_bound_rules} {path:?}"
        );
    }
}

#[tokio::test]
async fn an_action_on_a_row_that_no_longer_waits_sends_nothing() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    insert(
        &connection,
        session,
        vec![
            row("ask-1", Some("/usr/bin/curl")),
            row("ask-2", Some("/usr/bin/curl")),
            row("ask-3", Some("/usr/bin/curl")),
        ],
    );
    // Answered elsewhere, put off, withdrawn.
    pending_rows::observe(
        &connection,
        session,
        &ServerMessage::UpdateConnectionRows {
            rows: vec![
                ConnectionRow {
                    action: Some("allow".into()),
                    ..row("ask-1", Some("/usr/bin/curl"))
                },
                ConnectionRow {
                    deferred: true,
                    ..row("ask-2", Some("/usr/bin/curl"))
                },
            ],
        },
    );
    pending_rows::observe(
        &connection,
        session,
        &ServerMessage::RemoveConnectionRows {
            ids: vec!["ask-3".into()],
        },
    );
    for id in ["ask-1", "ask-2", "ask-3", "ask-9"] {
        for action in [NoticeAction::AllowOnce, NoticeAction::Deny] {
            assert_eq!(
                act(&handles, session, id, action),
                ActionOutcome::NoLongerWaiting,
                "{id} {action:?}"
            );
        }
    }
    assert!(inbound_rx.try_recv().is_err(), "nothing may be sent");

    // A new session that reuses the wire id: the old notice is stale, and a
    // message for the old session doesn't reach the new one's rows.
    insert(
        &connection,
        session,
        vec![row("ask-4", Some("/usr/bin/curl"))],
    );
    disconnect_and_discard(&connection, &mut inbound_rx);
    let next = mark_connected(&connection, true);
    insert(
        &connection,
        session,
        vec![row("ask-5", Some("/usr/bin/curl"))],
    );
    insert(&connection, next, vec![row("ask-4", Some("/usr/bin/curl"))]);
    assert_eq!(
        act(&handles, session, "ask-4", NoticeAction::AllowOnce),
        ActionOutcome::NoLongerWaiting
    );
    assert_eq!(handles.pending_row(next, "ask-5"), None);
    assert!(inbound_rx.try_recv().is_err(), "nothing may be sent");
    assert_eq!(
        act(&handles, next, "ask-4", NoticeAction::AllowOnce),
        ActionOutcome::Sent
    );
}

#[tokio::test]
async fn an_answer_that_cant_be_queued_says_so() {
    let (handles, connection, inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    insert(
        &connection,
        session,
        vec![row("ask-1", Some("/usr/bin/curl"))],
    );
    drop(inbound_rx);
    assert_eq!(
        act(&handles, session, "ask-1", NoticeAction::AllowOnce),
        ActionOutcome::NotSent
    );
}

#[tokio::test]
async fn a_pending_notice_is_announced_only_while_its_row_waits_in_its_session() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    let notice = |row_id| BridgeNotice::Pending {
        row_id,
        process: "curl".into(),
    };
    insert(
        &connection,
        session,
        vec![row("ask-1", Some("/usr/bin/curl")), row("ask-2", None)],
    );
    let (wire_id, waiting) = still_waiting(&handles, session, &notice(1)).expect("still waiting");
    assert_eq!(wire_id, "ask-1");
    assert_eq!(waiting.dst_host, "example.com");

    // Answered during the grace period, e.g. by a tray-only pause.
    pending_rows::observe(
        &connection,
        session,
        &ServerMessage::UpdateConnectionRows {
            rows: vec![ConnectionRow {
                action: Some("allow".into()),
                ..row("ask-1", Some("/usr/bin/curl"))
            }],
        },
    );
    assert!(still_waiting(&handles, session, &notice(1)).is_none());
    assert!(
        still_waiting(&handles, session, &notice(9)).is_none(),
        "an unknown row"
    );
    assert!(
        still_waiting(&handles, session, &BridgeNotice::DaemonAway).is_none(),
        "not a prompt"
    );

    // The session ended: its notice is stale even for a row that waited.
    disconnect_and_discard(&connection, &mut inbound_rx);
    assert!(still_waiting(&handles, session, &notice(2)).is_none());
}

/// What `act_and_confirm` reports for each way the bridge can settle the
/// row after an "Allow once" was sent.
#[tokio::test]
async fn the_bridges_update_tells_a_won_race_from_a_lost_one() {
    use crate::notification_actions::act_and_confirm;
    use snitchwatch_bridge::ws_messages::AutoAnswer;

    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    let decided = |action: &str| ConnectionRow {
        action: Some(action.into()),
        ..row("ask-1", Some("/usr/bin/curl"))
    };
    let update = |row: ConnectionRow| ServerMessage::UpdateConnectionRows { rows: vec![row] };
    let cases = [
        (update(decided("allow")), ActionOutcome::Sent),
        (update(decided("deny")), ActionOutcome::AlreadyAnswered),
        (
            update(ConnectionRow {
                auto_answer: Some(AutoAnswer::FilterPaused),
                ..decided("allow")
            }),
            ActionOutcome::AlreadyAnswered,
        ),
        (
            update(ConnectionRow {
                deferred: true,
                ..row("ask-1", Some("/usr/bin/curl"))
            }),
            ActionOutcome::AlreadyAnswered,
        ),
        (
            ServerMessage::RemoveConnectionRows {
                ids: vec!["ask-1".into()],
            },
            ActionOutcome::NoLongerWaiting,
        ),
    ];
    for (settling, expected) in cases {
        insert(
            &connection,
            session,
            vec![row("ask-1", Some("/usr/bin/curl"))],
        );
        let asking = handles.clone();
        let answer = tokio::spawn(async move {
            act_and_confirm(&asking, session, "ask-1", NoticeAction::AllowOnce).await
        });
        // Sent: so it is already listening for the update.
        inbound_rx.recv().await.expect("the answer was sent");
        handles
            .broadcast_tx
            .send(ReceivedServerMessage {
                connection_id: session,
                message: settling.clone(),
            })
            .unwrap();
        assert_eq!(answer.await.unwrap(), expected, "{settling:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn no_word_from_the_bridge_keeps_the_outcome() {
    use crate::notification_actions::act_and_confirm;

    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    insert(
        &connection,
        session,
        vec![row("ask-1", Some("/usr/bin/curl"))],
    );
    assert_eq!(
        act_and_confirm(&handles, session, "ask-1", NoticeAction::Deny).await,
        ActionOutcome::Sent
    );
    assert!(inbound_rx.try_recv().is_ok(), "the answer was sent");
    // A stale row is still refused at once.
    assert_eq!(
        act_and_confirm(&handles, session, "ask-9", NoticeAction::Deny).await,
        ActionOutcome::NoLongerWaiting
    );
}
