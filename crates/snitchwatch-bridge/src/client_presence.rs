//! Authenticated external clients, independent of internal broadcast receivers.
use crate::filter_pause::{FilterPause, PauseRequest, PauseState};
use std::future::Future;
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

    /// The current GUI-session generation. It only advances when the last
    /// authenticated session ends, so a session that reads it while holding
    /// its [`SessionLease`] gets a value that stays current until every GUI
    /// has left. `ws_server` stamps it on each pause request.
    pub fn current_generation(&self) -> u64 {
        self.state.lock().unwrap().loss_generation
    }

    /// Run `f` under the presence lock only while a GUI is authenticated and
    /// `generation` is still current: the check [`Admission::while_current`]
    /// makes, against a given generation.
    pub(crate) fn while_generation_current<T>(
        &self,
        generation: u64,
        f: impl FnOnce() -> T,
    ) -> Option<T> {
        let state = self.state.lock().unwrap();
        (state.clients > 0 && state.loss_generation == generation).then(f)
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
/// resync the tray and broadcast the new pause state. No
/// `FilterPauseExpired` notice: no GUI is left to show it. Ends when every
/// `ClientPresence` is dropped.
///
/// A pause request still queued when its GUI left is handled by
/// [`apply_pause_request`], which every pause must go through.
pub async fn clear_pause_on_last_session_loss<F, Fut>(
    mut losses: watch::Receiver<u64>,
    pause: Arc<FilterPause>,
    on_cleared: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    while losses.changed().await.is_ok() {
        // A late wake-up must not clear a pause a GUI of the new generation
        // set in the meantime; only the departed generation's pause goes.
        let current_generation = *losses.borrow_and_update();
        if pause.clear_if_owner_ended(current_generation) {
            tracing::info!("last authenticated GUI session ended; filtering pause cleared");
            on_cleared().await;
        }
    }
}

/// Apply a GUI's pause/resume request and return the pause state that took
/// effect. Every pause goes through here; nothing else calls
/// [`FilterPause::pause`].
///
/// A pause takes effect only while its sender's GUI session generation is
/// still current. `sender_generation` is the stamp `ws_server` puts on each
/// request from an authenticated WebSocket session. A request that was still
/// queued when every GUI left is ignored, even if another GUI has
/// authenticated since (issue #47). An unstamped request (`None`) comes from
/// an in-process sender with no WebSocket session; it applies only while a
/// GUI is authenticated.
///
/// The pause is set under the presence lock, so it is never active with zero
/// sessions: a racing last-session loss either prevents the set or follows
/// it and is cleared by [`clear_pause_on_last_session_loss`].
pub fn apply_pause_request(
    presence: &ClientPresence,
    pause: &FilterPause,
    request: PauseRequest,
    sender_generation: Option<u64>,
) -> PauseState {
    let PauseRequest::Pause(duration) = request else {
        pause.resume();
        return pause.state();
    };
    let outcome = match sender_generation {
        Some(generation) => {
            presence.while_generation_current(generation, || pause.pause(duration, generation))
        }
        None => presence.admit().and_then(|admission| {
            admission.while_current(|| pause.pause(duration, admission.generation))
        }),
    };
    match outcome {
        None => tracing::info!(
            ?sender_generation,
            "pause request ignored: its GUI session is gone"
        ),
        Some(Err(rejected)) => tracing::warn!(%rejected, "pause request rejected"),
        Some(Ok(_)) => {}
    }
    pause.state()
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
    /// The GUI-session generation this admission was taken under.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Serialize admission/verdict resolution against the last-client loss.
    pub(crate) fn while_current<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        self.presence.while_generation_current(self.generation, f)
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

    const THIRTY_MINUTES: std::time::Duration = std::time::Duration::from_secs(1800);

    fn pause_for_thirty_minutes() -> PauseRequest {
        PauseRequest::Pause(THIRTY_MINUTES)
    }

    fn spawn_clear_task(
        presence: &ClientPresence,
        pause: &Arc<FilterPause>,
    ) -> tokio::sync::mpsc::UnboundedReceiver<()> {
        let (cleared_tx, cleared_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(clear_pause_on_last_session_loss(
            presence.session_losses(),
            pause.clone(),
            move || {
                let cleared_tx = cleared_tx.clone();
                async move {
                    let _ = cleared_tx.send(());
                }
            },
        ));
        cleared_rx
    }

    #[tokio::test]
    async fn last_session_loss_clears_a_filtering_pause() {
        let presence = ClientPresence::default();
        let pause = Arc::new(FilterPause::new());
        let first = presence.authenticated_session();
        let second = presence.authenticated_session();
        pause
            .pause(THIRTY_MINUTES, presence.current_generation())
            .unwrap();
        let mut cleared_rx = spawn_clear_task(&presence, &pause);
        tokio::task::yield_now().await;

        drop(first);
        tokio::task::yield_now().await;
        assert!(
            pause.is_active_now(),
            "another GUI is still authenticated: its pause stands"
        );

        drop(second);
        tokio::time::timeout(std::time::Duration::from_secs(1), cleared_rx.recv())
            .await
            .expect("pause was not cleared after the last session ended")
            .unwrap();
        assert!(!pause.is_active_now());
    }

    #[test]
    fn a_pause_request_takes_effect_only_with_an_authenticated_gui() {
        // A pause queued by a GUI that has since disconnected must not take
        // effect with no GUI attached; the cleanup task already ran and would
        // never clear it (issue #47).
        let presence = ClientPresence::default();
        let pause = FilterPause::new();
        assert!(!apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), None).paused);
        assert!(!pause.is_active_now());

        let _gui = presence.authenticated_session();
        assert!(apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), None).paused);
        assert!(pause.is_active_now());
        assert!(!apply_pause_request(&presence, &pause, PauseRequest::Resume, None).paused);
        assert!(!pause.is_active_now());
    }

    #[test]
    fn a_stamped_pause_from_a_session_whose_generation_ended_is_ignored() {
        // Issue #47's remaining race: GUI A's pause is still queued when A
        // leaves, and GUI B authenticates before the pump applies it.
        let presence = ClientPresence::default();
        let pause = FilterPause::new();
        let gui_a = presence.authenticated_session();
        let stamp_a = presence.current_generation();
        drop(gui_a);
        let _gui_b = presence.authenticated_session();

        let state =
            apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), Some(stamp_a));
        assert!(!state.paused, "B must not inherit A's queued pause");
        assert!(!pause.is_active_now());

        // B's own stamped request, and an unstamped in-process request, apply.
        let stamp_b = presence.current_generation();
        assert!(
            apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), Some(stamp_b))
                .paused
        );
        pause.resume();
        assert!(apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), None).paused);
    }

    #[tokio::test]
    async fn a_late_last_loss_clear_keeps_a_new_guis_fresh_pause() {
        // Code review L2: the clear task only sees GUI A's departure after
        // GUI B authenticated and paused; B's pause must stand.
        let presence = ClientPresence::default();
        let pause = Arc::new(FilterPause::new());
        let losses = presence.session_losses();
        let gui_a = presence.authenticated_session();
        let stamp_a = presence.current_generation();
        apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), Some(stamp_a));
        drop(gui_a);
        let _gui_b = presence.authenticated_session();
        let stamp_b = presence.current_generation();
        assert!(
            apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), Some(stamp_b))
                .paused
        );

        let (cleared_tx, mut cleared_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(clear_pause_on_last_session_loss(
            losses,
            pause.clone(),
            move || {
                let cleared_tx = cleared_tx.clone();
                async move {
                    let _ = cleared_tx.send(());
                }
            },
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), cleared_rx.recv())
                .await
                .is_err(),
            "the late clear must not report clearing B's pause"
        );
        assert!(pause.is_active_now(), "B's fresh pause was wiped");
    }

    #[test]
    fn a_stamped_pause_applies_while_any_gui_of_its_generation_remains() {
        let presence = ClientPresence::default();
        let pause = FilterPause::new();
        let _gui_a = presence.authenticated_session();
        let gui_b = presence.authenticated_session();
        let stamp_b = presence.current_generation();
        drop(gui_b);
        assert!(
            apply_pause_request(&presence, &pause, pause_for_thirty_minutes(), Some(stamp_b))
                .paused
        );
    }

    #[test]
    fn a_rejected_duration_changes_nothing() {
        let presence = ClientPresence::default();
        let pause = FilterPause::new();
        let _gui = presence.authenticated_session();
        let request = PauseRequest::Pause(std::time::Duration::from_secs(7200));
        assert!(!apply_pause_request(&presence, &pause, request, None).paused);
        assert!(!pause.is_active_now());
    }

    #[tokio::test]
    async fn last_session_loss_without_a_pause_does_not_report_a_clear() {
        let presence = ClientPresence::default();
        let pause = Arc::new(FilterPause::new());
        let session = presence.authenticated_session();
        let mut cleared_rx = spawn_clear_task(&presence, &pause);
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
