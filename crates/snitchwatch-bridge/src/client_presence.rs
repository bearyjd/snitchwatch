//! Authenticated external clients, independent of internal broadcast receivers.
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
}
