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
/// `on_cleared` runs after each loss that actually cleared a pause; the bridge
/// uses it to resync the tray. Ends when every `ClientPresence` is dropped.
pub async fn clear_pause_on_last_session_loss<F, Fut>(
    presence: ClientPresence,
    paused: Arc<AtomicBool>,
    on_cleared: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut losses = presence.losses.subscribe();
    drop(presence);
    while losses.changed().await.is_ok() {
        if paused.swap(false, Ordering::SeqCst) {
            tracing::info!("last authenticated GUI session ended; filtering pause cleared");
            on_cleared().await;
        }
    }
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
            presence.clone(),
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

    #[tokio::test]
    async fn last_session_loss_without_a_pause_does_not_report_a_clear() {
        let presence = ClientPresence::default();
        let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (cleared_tx, mut cleared_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = presence.authenticated_session();
        tokio::spawn(clear_pause_on_last_session_loss(
            presence.clone(),
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
