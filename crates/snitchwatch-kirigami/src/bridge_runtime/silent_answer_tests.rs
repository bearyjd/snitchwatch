//! Silent mode (`crate::silent_mode`), through the runtime's session state:
//! a new pending row of this GUI's session is answered at arrival with the
//! chosen verdict, once-only for this host, and marked as needing a rule; a
//! visible window with silent mode off, or a row of another session, is not
//! answered. The mode is process-global, so these tests take one lock.

use super::*;
use crate::settings::SilentMode;
use crate::silent_mode::{answer_new_rows, needs_rule, set_state, Answered};
use snitchwatch_bridge::ws_messages::{
    ConnectionRow, VerdictAction, VerdictDuration, VerdictScope,
};

static MODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn pending(id: &str) -> ConnectionRow {
    ConnectionRow {
        id: id.into(),
        process: "curl".into(),
        process_path: Some("/usr/bin/curl".into()),
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
        decided_by_default: false,
    }
}

fn arrival(rows: Vec<ConnectionRow>) -> ServerMessage {
    ServerMessage::InsertConnectionRows { rows }
}

#[tokio::test]
async fn new_prompts_are_answered_once_for_this_host_by_the_chosen_verdict() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    let mut answered = Answered::default();

    // Silent Deny, window shown.
    set_state(SilentMode::Deny, true);
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("ask-1")]),
        &mut answered,
    );
    let queued = inbound_rx.try_recv().expect("a silent verdict was queued");
    assert_eq!(queued.connection_id, session);
    assert_eq!(
        queued.message,
        ClientMessage::SetVerdict {
            row_id: "ask-1".into(),
            verdict: VerdictAction::Deny,
            scope: VerdictScope::ThisHost,
            duration: Some(VerdictDuration::Once),
            remember: None,
        }
    );
    assert!(needs_rule(&format!("{session}:ask-1")));

    // A repeat of the same prompt (a snapshot resync) is not answered twice.
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("ask-1")]),
        &mut answered,
    );
    assert!(inbound_rx.try_recv().is_err());

    // A hidden window with silent mode off allows.
    set_state(SilentMode::Off, false);
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("ask-2")]),
        &mut answered,
    );
    let queued = inbound_rx.try_recv().expect("a hidden window answers");
    assert!(matches!(
        queued.message,
        ClientMessage::SetVerdict {
            verdict: VerdictAction::Allow,
            duration: Some(VerdictDuration::Once),
            ..
        }
    ));
    set_state(SilentMode::Off, true);
}

#[tokio::test]
async fn nothing_is_answered_while_the_window_is_shown_and_silent_mode_is_off() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    set_state(SilentMode::Off, true);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("ask-3")]),
        &mut answered,
    );
    assert!(inbound_rx.try_recv().is_err());
    assert!(!needs_rule(&format!("{session}:ask-3")));
}

#[tokio::test]
async fn only_this_guis_current_session_is_answered() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let old = mark_connected(&connection, true);
    disconnect_and_discard(&connection, &mut inbound_rx);
    let current = mark_connected(&connection, true);
    assert_ne!(old, current);
    set_state(SilentMode::Allow, true);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        old,
        &arrival(vec![pending("ask-4")]),
        &mut answered,
    );
    assert!(inbound_rx.try_recv().is_err(), "a stale session's prompt");
    assert!(!needs_rule(&format!("{old}:ask-4")));
    answer_new_rows(
        &handles,
        current,
        &arrival(vec![pending("ask-4")]),
        &mut answered,
    );
    assert!(
        inbound_rx.try_recv().is_ok(),
        "the current session's prompt"
    );
    set_state(SilentMode::Off, true);
}
