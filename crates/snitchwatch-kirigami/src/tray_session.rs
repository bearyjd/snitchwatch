//! Q04: the tray's state, each part tagged with the bridge session that sent
//! it, and the watch that forgets a session once it's no longer live
//! (`docs/superpowers/plans/2026-10-27-q04-honest-tray-health.md`).
//!
//! The cleanup reads the live session when it runs on the Qt thread, not when
//! the watcher noticed a change: a cleanup queued during a disconnect may run
//! after the next session's state arrived, and must keep that state.

use std::time::Duration;

use snitchwatch_bridge::filter_pause::PauseState;
use snitchwatch_bridge::tray_state::TrayState;
use tokio::sync::{oneshot, watch};

use crate::tray_health::FirewallSeen;

/// A value and the bridge session that sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tagged<T> {
    pub session: u64,
    pub value: T,
}

/// What the tray shows, by session. `None` until a session sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraySession {
    pub tray: Option<Tagged<TrayState>>,
    pub pause: Option<Tagged<PauseState>>,
    pub firewall: FirewallSeen,
}

impl TraySession {
    pub fn with_tray(&self, session: u64, value: TrayState) -> Self {
        Self {
            tray: Some(Tagged { session, value }),
            ..self.clone()
        }
    }

    pub fn with_pause(&self, session: u64, value: PauseState) -> Self {
        Self {
            pause: Some(Tagged { session, value }),
            ..self.clone()
        }
    }

    pub fn with_firewall(&self, firewall: FirewallSeen) -> Self {
        Self {
            firewall,
            ..self.clone()
        }
    }

    /// Only what `live` sent; everything from any other session is dropped.
    pub fn reconcile(&self, live: Option<u64>) -> Self {
        let is_live = |session: u64| live == Some(session);
        Self {
            tray: self.tray.clone().filter(|t| is_live(t.session)),
            pause: self.pause.clone().filter(|p| is_live(p.session)),
            firewall: if is_live(self.firewall.session()) {
                self.firewall.clone()
            } else {
                FirewallSeen::default()
            },
        }
    }
}

/// A cleanup to queue onto the Qt thread. `live` is read when the job runs,
/// never when it's made.
pub fn cleanup_job<L>(live: L) -> impl FnOnce(&TraySession) -> TraySession
where
    L: FnOnce() -> Option<u64>,
{
    move |state| state.reconcile(live())
}

/// What the watch compares between polls: whether the link is up, and the
/// latest session id, which moves on every connect. A whole session that
/// connects and disconnects between two polls still changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkKey {
    connected: bool,
    session: u64,
}

/// The key for a link `connected` or not, whose latest session id (live or
/// last) is `session`.
pub fn link_key(connected: bool, session: u64) -> LinkKey {
    LinkKey { connected, session }
}

/// Calls `reconcile` once at once (state may have been rendered before the
/// watch began), then whenever `link` changes, every `period`, until `stop`
/// fires or its sender is dropped with the controller. Also stops when
/// `reconcile` returns false (the Qt object is gone).
pub async fn watch_sessions<L, R>(
    link: L,
    reconcile: R,
    period: Duration,
    stop: oneshot::Receiver<()>,
) where
    L: Fn() -> LinkKey,
    R: FnMut() -> bool,
{
    // Never bumped: only `period` wakes this watch.
    let (_never, wake) = watch::channel(0);
    watch_sessions_woken(link, reconcile, wake, period, stop).await;
}

/// [`watch_sessions`], but also re-checks `link` whenever `wake` changes, so
/// `period` can be a long fallback instead of a poll: the link is checked
/// when it changes, not several times a second while idle.
pub async fn watch_sessions_woken<L, R>(
    link: L,
    mut reconcile: R,
    mut wake: watch::Receiver<u64>,
    period: Duration,
    mut stop: oneshot::Receiver<()>,
) where
    L: Fn() -> LinkKey,
    R: FnMut() -> bool,
{
    let mut seen = None;
    let mut first = true;
    let mut wake_open = true;
    loop {
        // Mark the count seen before reading the link, so a change made
        // after this read wakes the next wait.
        wake.borrow_and_update();
        let now = Some(link());
        if first || now != seen {
            first = false;
            seen = now;
            if !reconcile() {
                return;
            }
        }
        tokio::select! {
            _ = &mut stop => return,
            () = tokio::time::sleep(period) => {}
            changed = wake.changed(), if wake_open => wake_open = changed.is_ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray_health::FirewallReport;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    const PAUSED: PauseState = PauseState {
        paused: true,
        expires_at_unix_ms: Some(1_800_000_300_000),
    };

    fn firewall_off(session: u64) -> FirewallSeen {
        FirewallSeen::default().observe(session, FirewallReport::NotRunning, || {
            unreachable!("first sight reads no clock")
        })
    }

    fn session_a() -> TraySession {
        TraySession::default()
            .with_tray(1, TrayState::Pending(2))
            .with_pause(1, PAUSED)
            .with_firewall(firewall_off(1))
    }

    #[test]
    fn a_disconnect_forgets_the_tray_pause_and_firewall() {
        assert_eq!(session_a().reconcile(None), TraySession::default());
    }

    #[test]
    fn the_live_session_keeps_everything() {
        assert_eq!(session_a().reconcile(Some(1)), session_a());
    }

    /// A to B with no disconnect between: nothing of A survives, including
    /// its pause.
    #[test]
    fn a_session_replacement_forgets_the_old_session() {
        let replaced = session_a().reconcile(Some(2));
        assert_eq!(replaced, TraySession::default());
        assert_eq!(replaced.pause, None);
    }

    #[test]
    fn only_the_stale_parts_are_dropped() {
        let mixed = session_a().with_tray(2, TrayState::Idle);
        let kept = mixed.reconcile(Some(2));
        assert_eq!(
            kept.tray,
            Some(Tagged {
                session: 2,
                value: TrayState::Idle
            })
        );
        assert_eq!(kept.pause, None);
        assert_eq!(kept.firewall, FirewallSeen::default());
    }

    /// The watcher saw a disconnect (None) and queued a cleanup; B connected
    /// and delivered its state before the cleanup ran. B must survive.
    #[test]
    fn a_cleanup_queued_while_disconnected_keeps_the_next_sessions_state() {
        let live = Cell::new(None);
        let job = cleanup_job(|| live.get());
        live.set(Some(2));
        let with_b = session_a()
            .with_tray(2, TrayState::Idle)
            .with_pause(2, PAUSED)
            .with_firewall(firewall_off(2));
        assert_eq!(job(&with_b), with_b);
    }

    #[test]
    fn a_cleanup_still_drops_a_session_gone_when_it_runs() {
        let live = Cell::new(Some(1));
        let job = cleanup_job(|| live.get());
        live.set(None);
        assert_eq!(job(&session_a()), TraySession::default());
    }

    fn spawn_watch(
        live: Arc<AtomicU64>,
        calls: Arc<AtomicUsize>,
    ) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let (stop_tx, stop_rx) = oneshot::channel();
        let handle = tokio::spawn(watch_sessions(
            move || match live.load(Ordering::SeqCst) {
                0 => link_key(false, 0),
                id => link_key(true, id),
            },
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                true
            },
            Duration::from_millis(250),
            stop_rx,
        ));
        (stop_tx, handle)
    }

    /// Session A rendered and disconnected before the watch began: the first
    /// pass must reconcile anyway, though the watch never saw A live.
    #[tokio::test(start_paused = true)]
    async fn the_first_pass_reconciles_even_when_disconnected() {
        let live = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (_stop, _handle) = spawn_watch(live.clone(), calls.clone());
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_millis(1000)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an unchanged link reconciled again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn each_session_change_reconciles_once() {
        let live = Arc::new(AtomicU64::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let (_stop, _handle) = spawn_watch(live.clone(), calls.clone());
        tokio::task::yield_now().await;
        for (next, expected) in [(2, 2), (0, 3), (3, 4)] {
            live.store(next, Ordering::SeqCst);
            tokio::time::advance(Duration::from_millis(250)).await;
            tokio::task::yield_now().await;
            assert_eq!(calls.load(Ordering::SeqCst), expected, "live {next}");
        }
    }

    /// The controller owns the stop sender; dropping it ends the watch even
    /// though the session never changes.
    #[tokio::test(start_paused = true)]
    async fn dropping_the_controllers_stop_ends_the_watch() {
        let live = Arc::new(AtomicU64::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let (stop, handle) = spawn_watch(live, calls);
        tokio::task::yield_now().await;
        drop(stop);
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(handle.is_finished(), "the watch outlived its controller");
        assert!(handle.await.is_ok(), "the watch ended by panicking");
    }

    /// Disconnected at the first pass; session A then connects, renders and
    /// disconnects entirely between two polls. Both polls see no live
    /// session, but the next one must still clean up after A.
    #[tokio::test(start_paused = true)]
    async fn a_whole_session_between_two_polls_is_still_cleaned_up() {
        let link = Arc::new(std::sync::Mutex::new((false, 0_u64)));
        let calls = Arc::new(AtomicUsize::new(0));
        let (_stop, stop_rx) = oneshot::channel();
        let _handle = tokio::spawn(watch_sessions(
            {
                let link = link.clone();
                move || {
                    let (connected, session) = *link.lock().unwrap();
                    link_key(connected, session)
                }
            },
            {
                let calls = calls.clone();
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    true
                }
            },
            Duration::from_millis(250),
            stop_rx,
        ));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "first pass");
        *link.lock().unwrap() = (true, 1);
        *link.lock().unwrap() = (false, 1);
        tokio::time::advance(Duration::from_millis(250)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "session A came and went unreconciled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_gone_qt_object_ends_the_watch() {
        let (_stop, stop_rx) = oneshot::channel();
        let handle = tokio::spawn(watch_sessions(
            || link_key(true, 1),
            || false,
            Duration::from_millis(250),
            stop_rx,
        ));
        tokio::task::yield_now().await;
        assert!(handle.is_finished());
    }

    /// The production watch (perf/gui-idle-wakeups): a link change is seen at
    /// the wake, with no time passing, and an idle link is not re-read before
    /// the long fallback.
    #[tokio::test(start_paused = true)]
    async fn a_woken_watch_reacts_at_the_wake_and_does_not_poll_while_idle() {
        let live = Arc::new(AtomicU64::new(1));
        let reads = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let (wake_tx, wake_rx) = watch::channel(0_u64);
        let (_stop, stop_rx) = oneshot::channel();
        let (l, r, c) = (live.clone(), reads.clone(), calls.clone());
        let _handle = tokio::spawn(watch_sessions_woken(
            move || {
                r.fetch_add(1, Ordering::SeqCst);
                link_key(true, l.load(Ordering::SeqCst))
            },
            move || {
                c.fetch_add(1, Ordering::SeqCst);
                true
            },
            wake_rx,
            Duration::from_secs(30),
            stop_rx,
        ));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let reads_before = reads.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(25)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            reads_before,
            "the idle link was polled"
        );
        live.store(2, Ordering::SeqCst);
        wake_tx.send_modify(|n| *n += 1);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the wake did not reconcile"
        );
    }
}
