//! Timed filtering pause (issue #47).
//!
//! A pause is always one of [`ALLOWED_PAUSE_SECS`] long and ends on its own.
//! It also ends on an explicit resume, or when the last authenticated GUI
//! session ends (`client_presence::clear_pause_on_last_session_loss`). Every
//! pause request goes through `client_presence::apply_pause_request`; nothing
//! else may call [`FilterPause::pause`].
//!
//! A pause belongs to the GUI-session generation that set it (see
//! `client_presence::ClientPresence::current_generation`). `ask_rule` honors
//! it only for an admission of that same generation, so a GUI that
//! authenticates after every earlier GUI left is never auto-allowed under
//! their pause, even before the last-loss clear has run.
//!
//! A pause is active only while *both* of its deadlines are ahead. The
//! monotonic deadline doesn't move when the wall clock is stepped backwards;
//! the wall deadline still passes while the machine is suspended, when the
//! monotonic clock stands still.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::{Instant, MissedTickBehavior};

use crate::client_presence::Admission;
use crate::ws_messages::ServerMessage;

/// The only pause lengths the bridge accepts: 5 minutes, 30 minutes, 1 hour.
pub const ALLOWED_PAUSE_SECS: [u64; 3] = [300, 1800, 3600];

/// Length of a pause requested by a client that predates timed pauses
/// (`setFilteringPaused` without `durationSecs`).
pub const LEGACY_PAUSE_SECS: u64 = 300;

/// How often [`expire_pause_on_deadline`] checks the deadlines while a pause
/// is set. A short tick (rather than one long sleep) also notices a deadline
/// that passed while the machine was suspended.
pub const EXPIRY_TICK: Duration = Duration::from_secs(1);

/// A GUI's pause/resume request, decoded from `ClientMessage::SetFilteringPaused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseRequest {
    Resume,
    Pause(Duration),
}

impl PauseRequest {
    /// A pause without a duration comes from an older client and gets
    /// [`LEGACY_PAUSE_SECS`]: no pause is ever indefinite.
    pub fn from_wire(paused: bool, duration_secs: Option<u64>) -> Self {
        if paused {
            Self::Pause(Duration::from_secs(
                duration_secs.unwrap_or(LEGACY_PAUSE_SECS),
            ))
        } else {
            Self::Resume
        }
    }
}

/// A pause length outside [`ALLOWED_PAUSE_SECS`]. Nothing changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("pause of {0:?} rejected: only 5 min, 30 min or 1 hour are allowed")]
pub struct Rejected(pub Duration);

/// The pause state the bridge broadcasts as `ServerMessage::FilterPauseState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseState {
    pub paused: bool,
    /// When the pause ends on its own, in Unix milliseconds. `None` while
    /// not paused.
    pub expires_at_unix_ms: Option<u64>,
}

impl PauseState {
    pub const NOT_PAUSED: Self = Self {
        paused: false,
        expires_at_unix_ms: None,
    };

    pub fn to_message(self) -> ServerMessage {
        ServerMessage::FilterPauseState {
            paused: self.paused,
            expires_at_unix_ms: self.expires_at_unix_ms,
        }
    }
}

struct Active {
    mono_deadline: Instant,
    wall_deadline: SystemTime,
    /// The GUI-session generation whose request set this pause.
    owner_generation: u64,
}

impl Active {
    fn is_active(&self, now_mono: Instant, now_wall: SystemTime) -> bool {
        now_mono < self.mono_deadline && now_wall < self.wall_deadline
    }
}

type WallClock = Box<dyn Fn() -> SystemTime + Send + Sync>;

/// The bridge's single filtering-pause state. Its lock is a leaf: never call
/// into the client presence or the connection cache while holding it.
pub struct FilterPause {
    active: Mutex<Option<Active>>,
    wall_clock: WallClock,
    /// Wakes [`expire_pause_on_deadline`] when a pause starts, so it ticks
    /// only while one is set.
    started: tokio::sync::Notify,
}

impl Default for FilterPause {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterPause {
    pub fn new() -> Self {
        Self::with_wall_clock(SystemTime::now)
    }

    /// Tests inject a controllable wall clock to model suspend and clock steps.
    pub fn with_wall_clock(wall_clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        Self {
            active: Mutex::new(None),
            wall_clock: Box::new(wall_clock),
            started: tokio::sync::Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Active>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start (or replace) a pause owned by GUI-session generation
    /// `owner_generation`. Only `client_presence::apply_pause_request` may
    /// call this, under the presence lock.
    pub(crate) fn pause(
        &self,
        duration: Duration,
        owner_generation: u64,
    ) -> Result<PauseState, Rejected> {
        if duration.subsec_nanos() != 0 || !ALLOWED_PAUSE_SECS.contains(&duration.as_secs()) {
            return Err(Rejected(duration));
        }
        let now_mono = Instant::now();
        let now_wall = (self.wall_clock)();
        *self.lock() = Some(Active {
            mono_deadline: now_mono + duration,
            wall_deadline: now_wall + duration,
            owner_generation,
        });
        self.started.notify_one();
        Ok(self.state())
    }

    /// Whether a pause is set, expired or not.
    fn is_set(&self) -> bool {
        self.lock().is_some()
    }

    /// End any pause. Reports whether there was one to clear.
    pub fn resume(&self) -> bool {
        self.lock().take().is_some()
    }

    /// Clear a pause whose owning GUI-session generation has ended, i.e. one
    /// older than `current_generation`. A pause set since, by a GUI of the
    /// current generation, is kept. Reports whether a pause was cleared.
    pub fn clear_if_owner_ended(&self, current_generation: u64) -> bool {
        let mut active = self.lock();
        if active
            .as_ref()
            .is_some_and(|pause| pause.owner_generation < current_generation)
        {
            *active = None;
            return true;
        }
        false
    }

    /// Whether the pause applies to an `AskRule` admitted under `admission`:
    /// it is active, and owned by the admission's GUI-session generation,
    /// which is still current. Checked under the presence lock (lock order
    /// presence → pause), so a racing last-session loss can't slip between
    /// the check and the auto-allow decision.
    pub(crate) fn applies_to(&self, admission: &Admission) -> bool {
        let generation = admission.generation();
        admission.while_current(|| {
            let now_mono = Instant::now();
            let now_wall = (self.wall_clock)();
            self.lock().as_ref().is_some_and(|pause| {
                pause.owner_generation == generation && pause.is_active(now_mono, now_wall)
            })
        }) == Some(true)
    }

    pub fn is_active(&self, now_mono: Instant, now_wall: SystemTime) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|active| active.is_active(now_mono, now_wall))
    }

    pub fn is_active_now(&self) -> bool {
        self.is_active(Instant::now(), (self.wall_clock)())
    }

    /// The current state. An expired pause reads as not paused even before
    /// [`Self::take_expired`] clears it.
    pub fn state(&self) -> PauseState {
        let now_mono = Instant::now();
        let now_wall = (self.wall_clock)();
        match self.lock().as_ref() {
            Some(active) if active.is_active(now_mono, now_wall) => {
                // The earlier of the two deadlines, on the wall clock: after a
                // backwards step the monotonic deadline ends the pause first.
                let mono_left = active.mono_deadline - now_mono;
                let wall_left = active
                    .wall_deadline
                    .duration_since(now_wall)
                    .unwrap_or_default();
                PauseState {
                    paused: true,
                    expires_at_unix_ms: Some(unix_ms(now_wall + mono_left.min(wall_left))),
                }
            }
            _ => PauseState::NOT_PAUSED,
        }
    }

    /// Clear the pause if it has expired, checked and cleared under one lock
    /// so a resume or a fresh pause in between is never cleared by mistake.
    /// Reports whether an expired pause was cleared.
    pub fn take_expired(&self) -> bool {
        let now_mono = Instant::now();
        let now_wall = (self.wall_clock)();
        let mut active = self.lock();
        if active
            .as_ref()
            .is_some_and(|pause| !pause.is_active(now_mono, now_wall))
        {
            *active = None;
            return true;
        }
        false
    }
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Clear a pause once it expires and run `on_expired` (the bridge sends
/// `Notice::FilterPauseExpired`, broadcasts the new state and resyncs the
/// tray). Ticks every [`EXPIRY_TICK`] only while a pause is set and sleeps
/// otherwise. Runs until aborted.
pub async fn expire_pause_on_deadline<F, Fut>(pause: Arc<FilterPause>, on_expired: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        // `notify_one` keeps a permit when nobody waits, so a pause that
        // starts between the check and the wait is not missed.
        while !pause.is_set() {
            pause.started.notified().await;
        }
        let mut tick = tokio::time::interval(EXPIRY_TICK);
        // One check after a suspend is enough; don't replay every missed tick.
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        while pause.is_set() {
            tick.tick().await;
            if pause.take_expired() {
                tracing::info!("filtering pause expired");
                on_expired().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A wall clock tests can step independently of tokio's paused clock.
    #[derive(Clone)]
    struct TestWall(Arc<Mutex<SystemTime>>);

    impl TestWall {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(
                UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            )))
        }
        fn now(&self) -> SystemTime {
            *self.0.lock().unwrap()
        }
        fn step_forward(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
        fn step_back(&self, by: Duration) {
            *self.0.lock().unwrap() -= by;
        }
        fn pause(&self) -> Arc<FilterPause> {
            let wall = self.clone();
            Arc::new(FilterPause::with_wall_clock(move || wall.now()))
        }
        /// Both clocks move together, as they do while the machine is awake.
        async fn advance(&self, by: Duration) {
            self.step_forward(by);
            tokio::time::advance(by).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn allowed_durations_pause_until_their_deadline() {
        for secs in ALLOWED_PAUSE_SECS {
            let wall = TestWall::new();
            let pause = wall.pause();
            let state = pause.pause(Duration::from_secs(secs), 0).unwrap();
            assert_eq!(
                state,
                PauseState {
                    paused: true,
                    expires_at_unix_ms: Some(unix_ms(wall.now()) + secs * 1000),
                }
            );
            wall.advance(Duration::from_secs(secs) - Duration::from_millis(1))
                .await;
            assert!(pause.is_active_now(), "{secs} s pause ended early");
            wall.advance(Duration::from_millis(1)).await;
            assert!(
                !pause.is_active_now(),
                "{secs} s pause outlived its deadline"
            );
            assert_eq!(pause.state(), PauseState::NOT_PAUSED);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn other_durations_are_rejected_and_change_nothing() {
        let wall = TestWall::new();
        let pause = wall.pause();
        for secs in [0, 301, 7200] {
            assert_eq!(
                pause.pause(Duration::from_secs(secs), 0),
                Err(Rejected(Duration::from_secs(secs)))
            );
            assert!(!pause.is_active_now(), "{secs} s must not start a pause");
        }

        let before = pause.pause(Duration::from_secs(300), 0).unwrap();
        assert!(pause.pause(Duration::from_secs(7200), 0).is_err());
        assert_eq!(
            pause.state(),
            before,
            "a rejected request kept the pause as is"
        );
    }

    #[test]
    fn a_fractional_duration_is_rejected() {
        let pause = FilterPause::new();
        let fractional = Duration::new(300, 1);
        assert_eq!(pause.pause(fractional, 0), Err(Rejected(fractional)));
        assert!(!pause.is_active_now());
    }

    #[tokio::test(start_paused = true)]
    async fn a_wall_jump_past_the_deadline_ends_the_pause() {
        // Suspend: the monotonic clock stands still while the wall clock runs.
        let wall = TestWall::new();
        let pause = wall.pause();
        pause.pause(Duration::from_secs(300), 0).unwrap();
        wall.step_forward(Duration::from_secs(301));
        assert!(!pause.is_active_now());
        assert_eq!(pause.state(), PauseState::NOT_PAUSED);
    }

    #[tokio::test(start_paused = true)]
    async fn a_backwards_wall_jump_does_not_extend_the_pause() {
        let wall = TestWall::new();
        let pause = wall.pause();
        pause.pause(Duration::from_secs(300), 0).unwrap();
        wall.step_back(Duration::from_secs(3600));
        // The end time shown to the user follows the earlier deadline.
        assert_eq!(
            pause.state().expires_at_unix_ms,
            Some(unix_ms(wall.now()) + 300_000)
        );
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(!pause.is_active_now());
    }

    #[test]
    fn a_pause_never_applies_through_a_stale_admission() {
        // Security review F2: the last GUI leaves between `ask_rule`'s
        // admission and its pause check, before the clear task has run.
        let presence = crate::client_presence::ClientPresence::default();
        let pause = FilterPause::new();
        let gui = presence.authenticated_session();
        let admission = presence.admit().unwrap();
        pause
            .pause(Duration::from_secs(300), presence.current_generation())
            .unwrap();
        assert!(pause.applies_to(&admission));
        drop(gui);
        assert!(pause.is_active_now(), "the clear task hasn't run yet");
        assert!(!pause.applies_to(&admission));
    }

    #[test]
    fn a_pause_applies_only_to_its_owner_generation() {
        // Security review F1: GUI B authenticates after GUI A, the pause's
        // owner, left; until the clear task runs, A's pause must not
        // auto-allow for B.
        let presence = crate::client_presence::ClientPresence::default();
        let pause = FilterPause::new();
        let gui_a = presence.authenticated_session();
        pause
            .pause(Duration::from_secs(300), presence.current_generation())
            .unwrap();
        drop(gui_a);
        let _gui_b = presence.authenticated_session();
        assert!(!pause.applies_to(&presence.admit().unwrap()));

        assert!(pause.clear_if_owner_ended(presence.current_generation()));
        pause
            .pause(Duration::from_secs(300), presence.current_generation())
            .unwrap();
        assert!(pause.applies_to(&presence.admit().unwrap()));
        assert!(
            !pause.clear_if_owner_ended(presence.current_generation()),
            "B's own pause must survive a late clear for A's loss"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resume_reports_whether_a_pause_was_cleared() {
        let pause = TestWall::new().pause();
        assert!(!pause.resume());
        pause.pause(Duration::from_secs(1800), 0).unwrap();
        assert!(pause.resume());
        assert!(!pause.is_active_now());
        assert!(!pause.resume());
    }

    #[tokio::test(start_paused = true)]
    async fn take_expired_clears_only_an_expired_pause() {
        let wall = TestWall::new();
        let pause = wall.pause();
        assert!(!pause.take_expired());
        pause.pause(Duration::from_secs(300), 0).unwrap();
        assert!(!pause.take_expired());
        assert!(pause.is_active_now());
        wall.advance(Duration::from_secs(300)).await;
        assert!(pause.take_expired());
        assert!(!pause.take_expired(), "an expiry is reported once");
        assert!(!pause.resume(), "the expired pause was cleared");
    }

    #[test]
    fn a_request_without_a_duration_is_a_five_minute_pause() {
        assert_eq!(
            PauseRequest::from_wire(true, None),
            PauseRequest::Pause(Duration::from_secs(300))
        );
        assert_eq!(
            PauseRequest::from_wire(true, Some(3600)),
            PauseRequest::Pause(Duration::from_secs(3600))
        );
        assert_eq!(
            PauseRequest::from_wire(false, Some(3600)),
            PauseRequest::Resume
        );
    }

    /// Spawn the expiry loop with a callback that counts expiries.
    fn spawn_expiry(pause: &Arc<FilterPause>) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let expiries = Arc::new(AtomicUsize::new(0));
        let counter = expiries.clone();
        let task = tokio::spawn(expire_pause_on_deadline(pause.clone(), move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }));
        (expiries, task)
    }

    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    fn pending_row(id: &str) -> crate::ws_messages::ConnectionRow {
        crate::ws_messages::ConnectionRow {
            id: id.to_string(),
            process: "curl".to_string(),
            process_path: Some("/usr/bin/curl".to_string()),
            dst_host: "example.com".to_string(),
            dst_ip: "93.184.216.34".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
            matched_rule: None,
            auto_answer: None,
        }
    }

    fn paused_cache() -> (
        crate::cache::connections::ConnectionCache,
        Arc<FilterPause>,
        tokio::sync::watch::Receiver<crate::tray_state::TrayState>,
    ) {
        let tray = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let tray_rx = tray.subscribe();
        let pause = Arc::new(FilterPause::new());
        let cache = crate::cache::connections::ConnectionCache::with_tray_publisher(64, tray)
            .with_filter_pause(pause.clone());
        (cache, pause, tray_rx)
    }

    #[test]
    fn resolving_a_pre_pause_prompt_while_paused_keeps_the_tray_on_filter_off() {
        use crate::tray_state::TrayState;
        use crate::ws_messages::{VerdictDuration, VerdictScope};
        let (mut cache, pause, tray_rx) = paused_cache();
        let _verdict = cache.insert_pending(pending_row("pre-pause"));
        assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

        pause.pause(Duration::from_secs(300), 0).unwrap();
        cache.resync_tray_state();
        assert_eq!(
            *tray_rx.borrow(),
            TrayState::FilterOff,
            "pause outranks Pending(n)"
        );
        cache
            .resolve(
                "pre-pause",
                crate::cache::connections::Verdict::Deny,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .unwrap();
        assert_eq!(*tray_rx.borrow(), TrayState::FilterOff);
    }

    #[test]
    fn cancelling_a_prompt_while_paused_keeps_the_tray_on_filter_off() {
        use crate::tray_state::TrayState;
        let (mut cache, pause, tray_rx) = paused_cache();
        let _verdict = cache.insert_pending(pending_row("pre-pause"));
        pause.pause(Duration::from_secs(300), 0).unwrap();
        assert!(cache.cancel_pending("pre-pause"));
        assert_eq!(*tray_rx.borrow(), TrayState::FilterOff);

        pause.resume();
        cache.resync_tray_state();
        assert_eq!(*tray_rx.borrow(), TrayState::Idle);
    }

    /// Issue #58: a pause expiring during a daemon outage (the expiry
    /// announcement resyncs the tray) leaves the tray on `DaemonDown`.
    #[tokio::test(start_paused = true)]
    async fn a_pause_expiring_during_an_outage_keeps_the_tray_on_daemon_down() {
        use crate::tray_state::TrayState;
        let wall = TestWall::new();
        let pause = wall.pause();
        let tray = Arc::new(crate::tray_state::TrayStatePublisher::new());
        let tray_rx = tray.subscribe();
        let mut cache = crate::cache::connections::ConnectionCache::with_tray_publisher(64, tray)
            .with_filter_pause(pause.clone());
        let secs = ALLOWED_PAUSE_SECS[0];
        pause.pause(Duration::from_secs(secs), 0).unwrap();
        cache.set_daemon_down(true);
        cache.resync_tray_state();
        assert_eq!(*tray_rx.borrow(), TrayState::DaemonDown);

        wall.advance(Duration::from_secs(secs + 1)).await;
        assert!(pause.take_expired());
        cache.resync_tray_state();
        assert_eq!(*tray_rx.borrow(), TrayState::DaemonDown);

        cache.set_daemon_down(false);
        cache.resync_tray_state();
        assert_eq!(*tray_rx.borrow(), TrayState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_loop_reports_each_expiry_exactly_once() {
        let wall = TestWall::new();
        let pause = wall.pause();
        let (expiries, task) = spawn_expiry(&pause);

        wall.advance(Duration::from_secs(30)).await;
        settle().await;
        assert_eq!(
            expiries.load(Ordering::SeqCst),
            0,
            "silent while not paused"
        );

        pause.pause(Duration::from_secs(300), 0).unwrap();
        wall.advance(Duration::from_secs(299)).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 0);

        wall.advance(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 1);
        assert!(!pause.resume(), "the loop cleared the expired pause");

        wall.advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_loop_sleeps_while_nothing_is_paused() {
        // Code review L5: the 1 s tick runs only while a pause is set. Every
        // tick reads the wall clock, so a still clock-read count means no
        // wake-ups.
        let wall = TestWall::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let pause = {
            let (wall, reads) = (wall.clone(), reads.clone());
            Arc::new(FilterPause::with_wall_clock(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                wall.now()
            }))
        };
        let (expiries, task) = spawn_expiry(&pause);
        settle().await;
        let idle = reads.load(Ordering::SeqCst);
        wall.advance(Duration::from_secs(3600)).await;
        settle().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            idle,
            "woke with nothing paused"
        );

        pause.pause(Duration::from_secs(300), 0).unwrap();
        wall.advance(Duration::from_secs(301)).await;
        settle().await;
        assert_eq!(
            expiries.load(Ordering::SeqCst),
            1,
            "the pause still expires"
        );

        let after = reads.load(Ordering::SeqCst);
        wall.advance(Duration::from_secs(3600)).await;
        settle().await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            after,
            "kept ticking after the pause"
        );
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_re_pause_before_expiry_supersedes_the_old_deadline() {
        let wall = TestWall::new();
        let pause = wall.pause();
        let (expiries, task) = spawn_expiry(&pause);

        pause.pause(Duration::from_secs(300), 0).unwrap();
        wall.advance(Duration::from_secs(240)).await;
        pause.pause(Duration::from_secs(1800), 0).unwrap();
        wall.advance(Duration::from_secs(120)).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 0, "old deadline fired");
        assert!(pause.is_active_now());

        wall.advance(Duration::from_secs(1800)).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_pause_that_ends_during_suspend_expires_within_one_tick() {
        let wall = TestWall::new();
        let pause = wall.pause();
        let (expiries, task) = spawn_expiry(&pause);

        pause.pause(Duration::from_secs(300), 0).unwrap();
        settle().await;
        wall.step_forward(Duration::from_secs(400));
        tokio::time::advance(EXPIRY_TICK).await;
        settle().await;
        assert_eq!(expiries.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
