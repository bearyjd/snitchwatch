//! Silent mode (`crate::silent_mode`), through the runtime's session state:
//! a new pending row of this GUI's session is answered at arrival with the
//! chosen verdict, once-only for this host, and marked as needing a rule;
//! "Ask me" (whatever the main window is doing) or a row of another session
//! is not answered, and switching to Allow/Deny answers the rows already
//! waiting. The mode is process-global, so these tests take one lock.

use super::*;
use crate::settings::SilentMode;
use crate::silent_mode::{answer_new_rows, answer_pending_now, needs_rule, set_state, Answered};
use snitchwatch_bridge::ws_messages::{
    ConnectionRow, VerdictAction, VerdictDuration, VerdictScope,
};

// One lock with the recorder's own tests: both use the process-wide state.
use crate::learned::TEST_LOCK as MODE_LOCK;

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

    // Silent Deny.
    set_state(SilentMode::Deny);
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

    set_state(SilentMode::Off);
}

#[tokio::test]
async fn switching_to_a_silent_mode_answers_the_rows_already_waiting_once() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    let mut answered = Answered::default();
    set_state(SilentMode::Off);

    // "Ask me": the prompt arrives and waits for the alert.
    let message = arrival(vec![pending("ask-5"), pending("ask-6")]);
    pending_rows::observe(&connection, session, &message);
    answer_new_rows(&handles, session, &message, &mut answered);
    answer_pending_now(&handles, &mut answered);
    assert!(inbound_rx.try_recv().is_err(), "Ask me answers nothing");

    // Switching to Deny answers both, once, and a second sweep or a repeat of
    // the snapshot does not answer them again.
    set_state(SilentMode::Deny);
    answer_pending_now(&handles, &mut answered);
    let mut verdicts = Vec::new();
    while let Ok(queued) = inbound_rx.try_recv() {
        assert!(matches!(
            queued.message,
            ClientMessage::SetVerdict {
                verdict: VerdictAction::Deny,
                duration: Some(VerdictDuration::Once),
                ..
            }
        ));
        verdicts.push(queued.message);
    }
    assert_eq!(verdicts.len(), 2);
    assert!(needs_rule(&format!("{session}:ask-5")));
    assert!(needs_rule(&format!("{session}:ask-6")));
    answer_pending_now(&handles, &mut answered);
    answer_new_rows(&handles, session, &message, &mut answered);
    assert!(inbound_rx.try_recv().is_err());
    set_state(SilentMode::Off);
}

#[tokio::test]
async fn a_silent_answer_that_could_not_be_queued_is_not_marked_as_needing_a_rule() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    drop(inbound_rx);
    set_state(SilentMode::Allow);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("ask-7")]),
        &mut answered,
    );
    assert!(!needs_rule(&format!("{session}:ask-7")));
    set_state(SilentMode::Off);
}

#[tokio::test]
async fn ask_me_answers_nothing_whatever_the_main_window_is_doing() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    set_state(SilentMode::Off);
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
    set_state(SilentMode::Allow);
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
    set_state(SilentMode::Off);
}

// --- learning mode records what was really answered (phase L2) ---------------------
//
// The recorder (`crate::learned`) is process-wide, like the silent state, so
// these share `MODE_LOCK` and reset both.

use crate::learned;
use crate::silent_mode::{on_lagged, sweep_waiting};

fn learned_connections() -> u64 {
    learned::snapshot()
        .c
        .get(&crate::learned::path_key("/usr/bin/curl"))
        .map_or(0, |p| p.connections)
}

/// A fresh learning window, silent mode `mode`.
fn begin_learning(mode: SilentMode) {
    learned::set_active(false);
    learned::set_active(true);
    set_state(mode);
}

/// No window, and an empty record (a window's record outlives it, so it can
/// be reviewed; a new window clears it).
fn no_learning() {
    learned::set_active(true);
    learned::set_active(false);
}

fn end_learning() {
    learned::set_active(false);
    set_state(SilentMode::Off);
}

#[tokio::test]
async fn an_allow_is_recorded_once_after_it_was_sent() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    begin_learning(SilentMode::Allow);
    let mut answered = Answered::default();

    let message = arrival(vec![pending("lr-1"), pending("lr-2")]);
    answer_new_rows(&handles, session, &message, &mut answered);
    assert!(inbound_rx.try_recv().is_ok());
    assert_eq!(learned_connections(), 2);
    let seen = learned::snapshot();
    let curl = &seen.c[&crate::learned::path_key("/usr/bin/curl")];
    assert_eq!(curl.destinations["example.com"], 2);
    assert_eq!(curl.ports.iter().copied().collect::<Vec<_>>(), [443]);

    // A resync snapshot that repeats the rows, or a second sweep, counts nothing.
    answer_new_rows(&handles, session, &message, &mut answered);
    pending_rows::observe(&connection, session, &message);
    answer_pending_now(&handles, &mut answered);
    assert_eq!(learned_connections(), 2);
    end_learning();
}

#[tokio::test]
async fn a_deny_is_never_recorded_as_a_connection_that_was_allowed() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    begin_learning(SilentMode::Deny);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("lr-1")]),
        &mut answered,
    );
    assert!(inbound_rx.try_recv().is_ok(), "answered");
    assert_eq!(learned_connections(), 0);
    end_learning();
}

#[tokio::test]
async fn nothing_is_recorded_when_no_window_is_running() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    no_learning();
    set_state(SilentMode::Allow);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("lr-1")]),
        &mut answered,
    );
    assert!(
        inbound_rx.try_recv().is_ok(),
        "plain Allow and record still answers"
    );
    assert_eq!(learned_connections(), 0);
    end_learning();
}

#[tokio::test]
async fn nothing_is_answered_or_recorded_while_the_mode_is_forced_off() {
    // The forced-mode override: learning is still on disk, but the mode in
    // force is Off, so nothing is answered, and so nothing is recorded.
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    begin_learning(SilentMode::Off);
    let mut answered = Answered::default();
    let message = arrival(vec![pending("lr-1")]);
    pending_rows::observe(&connection, session, &message);
    answer_new_rows(&handles, session, &message, &mut answered);
    answer_pending_now(&handles, &mut answered);
    sweep_waiting(&handles, &mut answered);
    assert!(inbound_rx.try_recv().is_err(), "nothing was answered");
    assert_eq!(learned_connections(), 0);
    end_learning();
}

#[tokio::test]
async fn a_send_that_failed_is_not_recorded_and_its_replan_records_it_once() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    begin_learning(SilentMode::Allow);
    let mut answered = Answered::default();

    // The queue holds 8: the ninth answer cannot be queued.
    let ids: Vec<String> = (1..=9).map(|n| format!("lr-{n}")).collect();
    let message = arrival(ids.iter().map(|id| pending(id)).collect());
    pending_rows::observe(&connection, session, &message);
    answer_new_rows(&handles, session, &message, &mut answered);
    assert_eq!(learned_connections(), 8, "the failed one is not recorded");

    // Make room; the next sweep plans the forgotten row again.
    while inbound_rx.try_recv().is_ok() {}
    answer_pending_now(&handles, &mut answered);
    assert!(
        inbound_rx.try_recv().is_ok(),
        "the replanned answer was sent"
    );
    assert_eq!(learned_connections(), 9, "recorded now, once");
    answer_pending_now(&handles, &mut answered);
    sweep_waiting(&handles, &mut answered);
    assert_eq!(learned_connections(), 9, "and never again");
    end_learning();
}

#[tokio::test]
async fn an_answer_that_could_not_be_queued_at_all_records_nothing() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    drop(inbound_rx);
    begin_learning(SilentMode::Allow);
    let mut answered = Answered::default();
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("lr-1")]),
        &mut answered,
    );
    assert_eq!(learned_connections(), 0);
    end_learning();
}

#[tokio::test]
async fn the_start_of_learning_records_rows_already_allowed_and_answers_the_rest() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    no_learning();
    let mut answered = Answered::default();

    // Before learning: one row was allowed, one denied, one is still waiting.
    set_state(SilentMode::Allow);
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("lr-1")]),
        &mut answered,
    );
    set_state(SilentMode::Deny);
    answer_new_rows(
        &handles,
        session,
        &arrival(vec![pending("lr-2")]),
        &mut answered,
    );
    pending_rows::observe(
        &connection,
        session,
        &arrival(vec![pending("lr-1"), pending("lr-2"), pending("lr-3")]),
    );
    while inbound_rx.try_recv().is_ok() {}
    assert_eq!(learned_connections(), 0);

    // Learning begins in Allow: the sweep answers the waiting row and records
    // it with the one already allowed, but not the one that was denied.
    learned::set_active(true);
    set_state(SilentMode::Allow);
    sweep_waiting(&handles, &mut answered);
    let sent: Vec<_> = std::iter::from_fn(|| inbound_rx.try_recv().ok()).collect();
    assert_eq!(sent.len(), 1, "only the unanswered row is answered");
    assert_eq!(learned_connections(), 2);
    sweep_waiting(&handles, &mut answered);
    assert_eq!(learned_connections(), 2, "a second sweep adds nothing");
    end_learning();
}

// The mode lock is held across the wait on purpose: the spawned sweep reads the
// process-wide silent state, which no other test may change meanwhile.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_lag_marks_the_record_incomplete_and_sweeps_the_rows_that_were_missed() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    let session = mark_connected(&connection, true);
    no_learning();
    *crate::silent_mode::shared_answered()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Answered::default();

    // ask-1 was allowed (and not recorded: no window then); ask-2 arrived
    // while the feed task lagged, so it was never planned.
    set_state(SilentMode::Allow);
    {
        let mut shared = crate::silent_mode::shared_answered()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        answer_new_rows(
            &handles,
            session,
            &arrival(vec![pending("lr-1")]),
            &mut shared,
        );
    }
    pending_rows::observe(
        &connection,
        session,
        &arrival(vec![pending("lr-1"), pending("lr-2")]),
    );
    while inbound_rx.try_recv().is_ok() {}
    learned::set_active(true);
    assert!(!learned::status().incomplete);

    on_lagged(&handles);
    assert!(learned::status().incomplete, "the review must say so");
    for _ in 0..200 {
        if learned_connections() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        learned_connections(),
        2,
        "both rows are recorded after the sweep"
    );
    assert!(inbound_rx.try_recv().is_ok(), "the missed row was answered");
    end_learning();
    *crate::silent_mode::shared_answered()
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Answered::default();
}

#[tokio::test]
async fn a_lag_outside_a_window_marks_nothing() {
    let _lock = MODE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (handles, _connection, _inbound_rx) = handles_and_queue();
    no_learning();
    set_state(SilentMode::Off);
    on_lagged(&handles);
    assert!(!learned::status().incomplete);
}
