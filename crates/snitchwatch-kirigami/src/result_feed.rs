//! The bridge feed of the rule-command sheets' controllers (the rule
//! editor's and "Make a rule…"'s): `RuleCommandResult`s only, from the live
//! bridge session only, on the controller's Qt thread.
//!
//! Kept beside [`crate::bridge_dispatch`], whose [`run_feed`] it uses, so
//! that file stays small.

use std::pin::Pin;

use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};

use crate::bridge_dispatch::run_feed;
use crate::bridge_runtime::SendClientMessageError;
use crate::rule_commands::interests_rule_results;

/// Feed the bridge's `RuleCommandResult`s to `on_message`, on the Qt thread
/// of the object `qt_thread` belongs to, and only from the live bridge
/// session. Asks for a snapshot first only when `resync`: a feed of results
/// alone keeps no state a snapshot could restore, so both controllers pass
/// `false`. False without a bridge runtime.
pub fn spawn_result_feed<T>(
    qt_thread: cxx_qt::CxxQtThread<T>,
    label: &'static str,
    on_message: fn(Pin<&mut T>, ServerMessage),
    resync: bool,
) -> bool
where
    T: cxx_qt::Threading + 'static,
{
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!(feed = label, "bridge not running; no rule command results");
        return false;
    };
    let rx = handles.subscribe();
    let snapshots = handles.clone();
    let session_handles = handles.clone();
    handles.runtime().spawn(run_feed(
        rx,
        request_snapshot_if(resync, move || {
            snapshots.try_send(ClientMessage::RequestSnapshot)
        }),
        label,
        interests_rule_results,
        move |connection_id, message, _json| {
            let session_handles = session_handles.clone();
            let message = message.clone();
            let _ = qt_thread.queue(move |qobject| {
                from_current_session(
                    |id| session_handles.is_current_session(id),
                    connection_id,
                    message,
                    |message| on_message(qobject, message),
                );
            });
        },
    ));
    true
}

/// How a feed asks for a snapshot: `send`, or nothing at all (reported as
/// done, so [`run_feed`] stops trying) when not `resync`.
fn request_snapshot_if<S>(
    resync: bool,
    send: S,
) -> impl Fn() -> Result<(), SendClientMessageError> + Send + 'static
where
    S: Fn() -> Result<(), SendClientMessageError> + Send + 'static,
{
    move || if resync { send() } else { Ok(()) }
}

/// Runs `deliver` only for a message from the live bridge session
/// (`is_current`): a result from an older session answers a request whose
/// wait already ended when that session went away.
fn from_current_session<M>(
    is_current: impl Fn(u64) -> bool,
    connection_id: u64,
    message: M,
    deliver: impl FnOnce(M),
) {
    if is_current(connection_id) {
        deliver(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_result_from_another_session_is_not_delivered() {
        let delivered = std::cell::RefCell::new(Vec::new());
        let is_current = |id| id == 2;
        from_current_session(is_current, 1, "old", |m| delivered.borrow_mut().push(m));
        from_current_session(is_current, 2, "live", |m| delivered.borrow_mut().push(m));
        assert_eq!(*delivered.borrow(), vec!["live"]);
    }

    /// A results-only feed asks for no snapshot; with `resync` it asks once.
    #[tokio::test]
    async fn a_feed_without_resync_requests_no_snapshot() {
        for (resync, wanted) in [(false, 0), (true, 1)] {
            let (btx, brx) = tokio::sync::broadcast::channel(4);
            let asked = Arc::new(AtomicUsize::new(0));
            let counter = asked.clone();
            let feed = tokio::spawn(run_feed(
                brx,
                request_snapshot_if(resync, move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
                "test-results",
                interests_rule_results,
                |_, _, _| {},
            ));
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            drop(btx);
            let _ = feed.await;
            assert_eq!(asked.load(Ordering::SeqCst), wanted, "resync {resync}");
        }
    }
}
