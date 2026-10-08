//! The bridge feed of the rule-command sheets' controllers (the rule
//! editor's and "Make a rule…"'s): `RuleCommandResult`s only, from the live
//! bridge session only, on the controller's Qt thread.
//!
//! Kept beside [`crate::bridge_dispatch`], whose [`run_feed`] it uses, so
//! that file stays small.

use std::pin::Pin;

use snitchwatch_bridge::ws_messages::ServerMessage;

use crate::bridge_dispatch::run_feed;
use crate::bridge_runtime::SendClientMessageError;
use crate::rule_commands::interests_rule_results;

/// Feed the bridge's `RuleCommandResult`s to `on_message`, on the Qt thread
/// of the object `qt_thread` belongs to, and only from the live bridge
/// session. It never asks for a snapshot (PR #111 review, L8: there is no
/// flag to ask for one): a feed of results alone keeps no state a snapshot
/// could restore. Without a bridge runtime it logs and does nothing.
pub fn spawn_result_feed<T>(
    qt_thread: cxx_qt::CxxQtThread<T>,
    label: &'static str,
    on_message: fn(Pin<&mut T>, ServerMessage),
) where
    T: cxx_qt::Threading + 'static,
{
    let Some(handles) = crate::bridge_runtime::handles() else {
        tracing::warn!(feed = label, "bridge not running; no rule command results");
        return;
    };
    let rx = handles.subscribe();
    let session_handles = handles.clone();
    handles.runtime().spawn(run_feed(
        rx,
        no_snapshot,
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
}

/// A result feed's snapshot request: none, reported as done so [`run_feed`]
/// stops trying (and asks for none after a lag either).
fn no_snapshot() -> Result<(), SendClientMessageError> {
    Ok(())
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

    #[test]
    fn a_result_from_another_session_is_not_delivered() {
        let delivered = std::cell::RefCell::new(Vec::new());
        let is_current = |id| id == 2;
        from_current_session(is_current, 1, "old", |m| delivered.borrow_mut().push(m));
        from_current_session(is_current, 2, "live", |m| delivered.borrow_mut().push(m));
        assert_eq!(*delivered.borrow(), vec!["live"]);
    }
}
