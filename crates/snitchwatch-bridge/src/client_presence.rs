//! Authenticated external clients, independent of internal broadcast receivers.
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Default)]
struct State {
    clients: usize,
    loss_generation: u64,
}

#[derive(Clone)]
pub struct ClientPresence {
    state: Arc<Mutex<State>>,
    losses: watch::Sender<u64>,
}

impl Default for ClientPresence {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            losses: watch::channel(0).0,
        }
    }
}

impl ClientPresence {
    /// Call only after token validation and a successful authentication ACK.
    pub fn authenticated_session(&self) -> SessionLease {
        self.state.lock().unwrap().clients += 1;
        SessionLease(self.clone())
    }

    /// Changes each time the last authenticated session ends. Subscribe
    /// before spawning a watcher so no loss slips past an unpolled task.
    pub fn session_losses(&self) -> watch::Receiver<u64> {
        self.losses.subscribe()
    }

    pub fn admit(&self) -> Option<Admission> {
        let state = self.state.lock().unwrap();
        (state.clients > 0).then(|| Admission {
            presence: self.clone(),
            generation: state.loss_generation,
            losses: self.losses.subscribe(),
        })
    }
}

/// Issue #47: clear a filtering pause whenever the last authenticated GUI
/// session ends. A pause is that GUI user's choice; left set, it would re-arm
/// for whichever GUI authenticates next (in system mode possibly a different
/// `snitchwatch-ui` member) while the tray may already show Idle.
/// `losses` comes from [`ClientPresence::session_losses`]. `on_cleared` runs
/// after each loss that actually cleared a pause; the bridge uses it to
/// resync the tray. Ends when every `ClientPresence` is dropped.
///
/// A pause request still queued when its GUI left is handled by
/// [`apply_pause_request`], which every pause must go through.
pub async fn clear_pause_on_last_session_loss<F, Fut>(
    mut losses: watch::Receiver<u64>,
    paused: Arc<AtomicBool>,
    on_cleared: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    while losses.changed().await.is_ok() {
        if paused.swap(false, Ordering::SeqCst) {
            tracing::info!("last authenticated GUI session ended; filtering pause cleared");
            on_cleared().await;
        }
    }
}

/// Apply a GUI's pause/resume request and return the pause state that took
/// effect. A pause only takes effect while a GUI is authenticated: a request
/// still queued when its sender disconnected arrives after
/// [`clear_pause_on_last_session_loss`] already ran, so it would otherwise
/// re-arm the pause with no GUI attached (issue #47). The flag is set
/// *before* the presence check, so a last-session loss racing this call is
/// caught either here or by the cleanup task.
///
/// Remaining gap: if another GUI authenticates before a departed GUI's
/// queued pause is applied, the new GUI inherits that pause. Closing it
/// needs per-session message tagging.
pub fn apply_pause_request(
    presence: &ClientPresence,
    paused: &AtomicBool,
    requested: bool,
) -> bool {
    paused.store(requested, Ordering::SeqCst);
    if requested && presence.admit().is_none() {
        paused.store(false, Ordering::SeqCst);
        tracing::info!("pause request ignored: no authenticated GUI session");
        return false;
    }
    requested
}

pub struct SessionLease(ClientPresence);

impl Drop for SessionLease {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.clients -= 1;
        if state.clients == 0 {
            state.loss_generation += 1;
            self.0.losses.send_replace(state.loss_generation);
        }
    }
}

#[derive(Clone)]
pub struct Admission {
    presence: ClientPresence,
    generation: u64,
    losses: watch::Receiver<u64>,
}

impl Admission {
    /// Serialize admission/verdict resolution against the last-client loss.
    pub(crate) fn while_current<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let state = self.presence.state.lock().unwrap();
        (state.clients > 0 && state.loss_generation == self.generation).then(f)
    }

    pub async fn lost(&mut self) {
        loop {
            if *self.losses.borrow_and_update() != self.generation {
                return;
            }
            if self.losses.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn last_loss_is_latched_across_immediate_reconnect() {
        let presence = ClientPresence::default();
        assert!(presence.admit().is_none());
        let first = presence.authenticated_session();
        let second = presence.authenticated_session();
        let mut old = presence.admit().unwrap();
        drop(first);
        assert!(old.while_current(|| ()).is_some());
        drop(second);
        let _new = presence.authenticated_session();
        assert!(old.while_current(|| ()).is_none());
        tokio::time::timeout(std::time::Duration::from_secs(1), old.lost())
            .await
            .unwrap();
        assert!(presence.admit().unwrap().while_current(|| ()).is_some());
    }

    #[tokio::test]
    async fn last_session_loss_clears_a_filtering_pause() {
        let presence = ClientPresence::default();
        let paused = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (cleared_tx, mut cleared_rx) = tokio::sync::mpsc::unbounded_channel();
        let first = presence.authenticated_session();
        let second = presence.authenticated_session();
        tokio::spawn(clear_pause_on_last_session_loss(
            presence.session_losses(),
            paused.clone(),
            move || {
                let cleared_tx = cleared_tx.clone();
                async move {
                    let _ = cleared_tx.send(());
                }
            },
        ));
        tokio::task::yield_now().await;

        drop(first);
        tokio::task::yield_now().await;
        assert!(
            paused.load(std::sync::atomic::Ordering::SeqCst),
            "another GUI is still authenticated: its pause stands"
        );

        drop(second);
        tokio::time::timeout(std::time::Duration::from_secs(1), cleared_rx.recv())
            .await
            .expect("pause was not cleared after the last session ended")
            .unwrap();
        assert!(!paused.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn a_pause_request_takes_effect_only_with_an_authenticated_gui() {
        // A pause queued by a GUI that has since disconnected must not take
        // effect with no GUI attached; the cleanup task already ran and would
        // never clear it (issue #47).
        let presence = ClientPresence::default();
        let paused = std::sync::atomic::AtomicBool::new(false);
        assert!(!apply_pause_request(&presence, &paused, true));
        assert!(!paused.load(std::sync::atomic::Ordering::SeqCst));

        let _gui = presence.authenticated_session();
        assert!(apply_pause_request(&presence, &paused, true));
        assert!(paused.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!apply_pause_request(&presence, &paused, false));
        assert!(!paused.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn last_session_loss_without_a_pause_does_not_report_a_clear() {
        let presence = ClientPresence::default();
        let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (cleared_tx, mut cleared_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = presence.authenticated_session();
        tokio::spawn(clear_pause_on_last_session_loss(
            presence.session_losses(),
            paused.clone(),
            move || {
                let cleared_tx = cleared_tx.clone();
                async move {
                    let _ = cleared_tx.send(());
                }
            },
        ));
        tokio::task::yield_now().await;
        drop(session);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), cleared_rx.recv())
                .await
                .is_err(),
            "no pause was set, so nothing should be reported cleared"
        );
    }
}
