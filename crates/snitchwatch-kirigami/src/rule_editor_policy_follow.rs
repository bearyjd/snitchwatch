//! Keeps a controller's copy of the live session's daemon policy
//! (`DaemonPolicy`) current while nothing is being checked: a disconnect, a
//! new session or a replaced daemon wakes `bridge_runtime::link_wake`, and
//! the policy is re-read then, so a sheet left open shows what the daemon
//! now refuses.
//!
//! Kept out of `rule_editor_controller.rs` so that file stays small.

use std::pin::Pin;

use snitchwatch_bridge::rule_policy::PolicyContext;

/// Call `on_policy` with the runtime's daemon policy, on the Qt thread of the
/// object `qt_thread` belongs to: once now and after every link change. The
/// policy is read inside the queued call, on the Qt thread, so a read made
/// earlier can't go stale. Ends when that object is gone. Without a bridge
/// runtime it does nothing.
pub fn spawn_policy_follow<T>(
    qt_thread: cxx_qt::CxxQtThread<T>,
    on_policy: fn(Pin<&mut T>, PolicyContext),
) where
    T: cxx_qt::Threading + 'static,
{
    let Some(handles) = crate::bridge_runtime::handles() else {
        return;
    };
    let mut link_rx = crate::bridge_runtime::link_wake::subscribe();
    let runtime = handles.runtime().clone();
    runtime.spawn(async move {
        loop {
            let handles = handles.clone();
            if qt_thread
                .queue(move |qobject| on_policy(qobject, handles.daemon_policy()))
                .is_err()
            {
                break;
            }
            if link_rx.changed().await.is_err() {
                break;
            }
        }
    });
}
