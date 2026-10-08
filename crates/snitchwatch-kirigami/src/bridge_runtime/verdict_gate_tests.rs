//! Issue #72, end to end through `bridge_feed::dispatch_to`, the one path
//! every verdict takes. Whatever the caller asked for, a remembered verdict is
//! queued once-only unless the program's file can be bound (the caller's
//! `bindable_process_path`) and, for "This host only" and "Any host on this
//! domain", the session advertised app-bound rules. (Kept out of `tests.rs`,
//! which is near the 800-line cap.)

use super::*;
use snitchwatch_bridge::ws_messages::{VerdictAction, VerdictDuration, VerdictScope};

const ACTIONS: [VerdictAction; 2] = [VerdictAction::Allow, VerdictAction::Deny];
const FOREVER: Option<VerdictDuration> = Some(VerdictDuration::Always);

fn verdict(
    row_id: &str,
    action: VerdictAction,
    scope: VerdictScope,
    duration: Option<VerdictDuration>,
) -> ClientMessage {
    ClientMessage::SetVerdict {
        row_id: row_id.to_string(),
        verdict: action,
        scope,
        duration,
        remember: None,
    }
}

/// The duration of the next queued verdict.
async fn queued_duration(inbound_rx: &mut mpsc::Receiver<QueuedClientMessage>) -> VerdictDuration {
    match inbound_rx
        .recv()
        .await
        .expect("a verdict was queued")
        .message
    {
        ClientMessage::SetVerdict {
            duration, remember, ..
        } => snitchwatch_bridge::ws_messages::effective_verdict_duration(duration, remember),
        other => panic!("expected SetVerdict, got {other:?}"),
    }
}

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

#[tokio::test]
async fn only_a_capable_session_gets_a_remembered_host_scoped_verdict() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();

    // Session 1: an old bridge (bare acknowledgement). Allow and Deny alike.
    mark_connected(&connection, false);
    for action in ACTIONS {
        for (scope, expected) in [
            (VerdictScope::ThisHost, VerdictDuration::Once),
            (VerdictScope::AnyHostOnDomain, VerdictDuration::Once),
            (VerdictScope::AnyHost, VerdictDuration::Always),
        ] {
            let msg = verdict("1:7", action.clone(), scope, FOREVER);
            crate::bridge_feed::dispatch_to(&handles, msg, true).unwrap();
            assert_eq!(
                queued_duration(&mut inbound_rx).await,
                expected,
                "{action:?} {scope:?}"
            );
        }
    }
    let legacy = ClientMessage::SetVerdict {
        row_id: "1:8".to_string(),
        verdict: VerdictAction::Allow,
        scope: VerdictScope::ThisHost,
        duration: None,
        remember: Some(true),
    };
    crate::bridge_feed::dispatch_to(&handles, legacy, true).unwrap();
    assert_eq!(
        queued_duration(&mut inbound_rx).await,
        VerdictDuration::Once
    );

    // Session 2: a bridge that advertised app-bound rules keeps the choice.
    disconnect_and_discard(&connection, &mut inbound_rx);
    mark_connected(&connection, true);
    for action in ACTIONS {
        let msg = verdict("2:7", action.clone(), VerdictScope::ThisHost, FOREVER);
        crate::bridge_feed::dispatch_to(&handles, msg, true).unwrap();
        assert_eq!(
            queued_duration(&mut inbound_rx).await,
            VerdictDuration::Always,
            "{action:?}"
        );
    }
}

#[tokio::test]
async fn an_unidentifiable_program_is_never_remembered_under_any_scope() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();

    // Session 1 is old, session 2 capable: "Any host" would otherwise be kept
    // on the first and every scope on the second.
    for (session, app_bound_rules) in [(1, false), (2, true)] {
        if session == 2 {
            disconnect_and_discard(&connection, &mut inbound_rx);
        }
        mark_connected(&connection, app_bound_rules);
        for action in ACTIONS {
            for scope in [
                VerdictScope::ThisHost,
                VerdictScope::AnyHostOnDomain,
                VerdictScope::AnyHost,
            ] {
                let msg = verdict(&format!("{session}:7"), action.clone(), scope, FOREVER);
                crate::bridge_feed::dispatch_to(&handles, msg, false).unwrap();
                assert_eq!(
                    queued_duration(&mut inbound_rx).await,
                    VerdictDuration::Once,
                    "session {session} {action:?} {scope:?}"
                );
            }
        }
    }
}

/// Captures what `tracing` writes while a closure runs on this thread.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;
    fn make_writer(&'a self) -> LogCapture {
        self.clone()
    }
}

fn logged_while(f: impl FnOnce()) -> String {
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let logged = capture.0.lock().unwrap().clone();
    String::from_utf8(logged).unwrap()
}

#[tokio::test]
async fn a_downgrade_is_logged_with_its_row_and_scope_and_a_kept_verdict_is_not() {
    let (handles, connection, mut inbound_rx) = handles_and_queue();
    mark_connected(&connection, false);

    let downgraded = verdict(
        "1:7",
        VerdictAction::Allow,
        VerdictScope::AnyHostOnDomain,
        FOREVER,
    );
    let logged = logged_while(|| {
        crate::bridge_feed::dispatch_to(&handles, downgraded, true).unwrap();
    });
    assert!(
        logged.contains("WARN") && logged.contains("1:7") && logged.contains("AnyHostOnDomain"),
        "a downgrade must say which row and scope: {logged:?}"
    );
    assert_eq!(
        queued_duration(&mut inbound_rx).await,
        VerdictDuration::Once
    );

    // Nothing changed, so nothing to report: an "Any host" answer on an old
    // bridge, and an unidentifiable program that already asked for once-only.
    let kept = [
        (
            verdict("1:7", VerdictAction::Deny, VerdictScope::AnyHost, FOREVER),
            true,
        ),
        (
            verdict(
                "1:7",
                VerdictAction::Deny,
                VerdictScope::ThisHost,
                Some(VerdictDuration::Once),
            ),
            false,
        ),
    ];
    let logged = logged_while(|| {
        for (msg, bindable) in kept {
            crate::bridge_feed::dispatch_to(&handles, msg, bindable).unwrap();
        }
    });
    assert_eq!(logged, "", "a kept verdict logged a warning");
}
