//! The learning window's pure rules (phase L1): the injectable clocks, when a
//! window is over, how its running time advances, and how it ends. No I/O and
//! no Qt; `crate::settings_learning` applies these under the settings lock.
//!
//! A window can only END EARLY. It is over when the wall clock passes
//! `ends_ms` OR the saved running time reaches `duration_ms`, whichever comes
//! first, so setting the clock back cannot stretch it, and a suspend (which
//! the monotonic clock does not count but the wall clock does) cannot either.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::settings::{Learning, Settings};

/// The two clocks the window needs, injected so tests control time.
pub trait Clocks: Send + Sync {
    /// Wall-clock milliseconds since the Unix epoch (can be set backwards).
    fn wall_ms(&self) -> i64;
    /// Monotonic milliseconds from an arbitrary fixed start (only differences
    /// mean anything).
    fn monotonic_ms(&self) -> i64;
}

/// The real clocks.
pub struct SystemClocks;

impl Clocks for SystemClocks {
    fn wall_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
            // A clock before 1970 is a broken clock: report it as such.
            Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |v| -v),
        }
    }

    fn monotonic_ms(&self) -> i64 {
        static ANCHOR: OnceLock<Instant> = OnceLock::new();
        let elapsed = ANCHOR.get_or_init(Instant::now).elapsed();
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    }
}

/// Whether `learning` is over at wall-clock `now_wall_ms`.
pub fn is_expired(learning: &Learning, now_wall_ms: i64) -> bool {
    now_wall_ms >= learning.ends_ms || learning.elapsed_ms >= learning.duration_ms
}

/// `learning` with `running_delta_ms` of running time added (never negative).
pub fn advance(learning: &Learning, running_delta_ms: i64) -> Learning {
    Learning {
        elapsed_ms: learning.elapsed_ms.saturating_add(running_delta_ms.max(0)),
        ..learning.clone()
    }
}

/// `settings` with the window over: `silent_mode` goes back to what it was
/// before (`None` stays unchosen) and `learning` is cleared. Without a window
/// it changes nothing, so ending twice is the same as ending once.
pub fn end(settings: Settings) -> Settings {
    match &settings.learning {
        Some(learning) => Settings {
            silent_mode: learning.previous_mode,
            learning: None,
            ..settings
        },
        None => settings,
    }
}

/// `settings` with the window over because the owner changed the silent mode
/// by hand: the window is cleared and the owner's mode is kept.
pub fn end_keeping_mode(settings: Settings) -> Settings {
    Settings {
        learning: None,
        ..settings
    }
}

/// Running time since the last reading, from a monotonic clock.
#[derive(Debug, Default)]
pub struct Ticker {
    last_ms: Option<i64>,
}

impl Ticker {
    pub const fn new() -> Self {
        Self { last_ms: None }
    }

    /// Start counting from `monotonic_ms` unless counting already started.
    pub fn prime(&mut self, monotonic_ms: i64) {
        self.last_ms.get_or_insert(monotonic_ms);
    }

    /// Milliseconds of running time since the previous call (0 on the first,
    /// which only primes it; never negative).
    pub fn delta(&mut self, monotonic_ms: i64) -> i64 {
        let delta = self.last_ms.map_or(0, |last| (monotonic_ms - last).max(0));
        self.last_ms = Some(monotonic_ms);
        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{SilentMode, LEARNING_DEFAULT_MS};
    use std::sync::atomic::{AtomicI64, Ordering};

    /// Hand-set clocks.
    struct Fake {
        wall: AtomicI64,
        mono: AtomicI64,
    }

    impl Clocks for Fake {
        fn wall_ms(&self) -> i64 {
            self.wall.load(Ordering::SeqCst)
        }
        fn monotonic_ms(&self) -> i64 {
            self.mono.load(Ordering::SeqCst)
        }
    }

    const START: i64 = 1_000_000;

    fn win() -> Learning {
        Learning::begin(START, LEARNING_DEFAULT_MS, Some(SilentMode::Deny))
    }

    #[test]
    fn a_fresh_window_is_not_over_and_ends_at_the_wall_clock_bound() {
        let l = win();
        assert!(!is_expired(&l, START));
        assert!(!is_expired(&l, l.ends_ms - 1));
        assert!(is_expired(&l, l.ends_ms));
        assert!(is_expired(&l, l.ends_ms + 1));
    }

    #[test]
    fn a_clock_set_back_cannot_stretch_the_window() {
        // The wall clock says it is long before the end, but the GUI has run
        // for the whole duration: the elapsed bound ends it.
        let l = Learning {
            elapsed_ms: LEARNING_DEFAULT_MS,
            ..win()
        };
        assert!(is_expired(&l, START - 10 * LEARNING_DEFAULT_MS));
        let almost = Learning {
            elapsed_ms: LEARNING_DEFAULT_MS - 1,
            ..win()
        };
        assert!(!is_expired(&almost, START - 10 * LEARNING_DEFAULT_MS));
        assert!(is_expired(
            &advance(&almost, 1),
            START - 10 * LEARNING_DEFAULT_MS
        ));
    }

    #[test]
    fn a_suspend_gap_ends_it_by_the_wall_clock() {
        // Asleep for two days: the monotonic clock did not count it, the wall
        // clock did.
        let l = Learning {
            elapsed_ms: 60_000,
            ..win()
        };
        assert!(is_expired(&l, START + 2 * LEARNING_DEFAULT_MS));
    }

    #[test]
    fn a_zero_or_over_window_is_expired_at_once() {
        assert!(is_expired(&Learning::begin(START, 0, None), START));
        let hostile = Learning {
            elapsed_ms: -1,
            ..win()
        }
        .clamped();
        assert!(is_expired(&hostile, 0));
    }

    #[test]
    fn advance_adds_running_time_and_never_goes_negative() {
        let l = win();
        assert_eq!(advance(&l, 60_000).elapsed_ms, 60_000);
        assert_eq!(advance(&advance(&l, 60_000), 5).elapsed_ms, 60_005);
        assert_eq!(advance(&l, -9_999).elapsed_ms, 0);
        assert_eq!(advance(&l, 0), l);
        let full = Learning {
            elapsed_ms: i64::MAX,
            ..win()
        };
        assert_eq!(advance(&full, i64::MAX).elapsed_ms, i64::MAX);
        // The input is left alone.
        assert_eq!(l.elapsed_ms, 0);
    }

    fn settings_in_window(previous: Option<SilentMode>) -> Settings {
        Settings {
            silent_mode: Some(SilentMode::Allow),
            learning: Some(Learning {
                previous_mode: previous,
                ..win()
            }),
            ..Settings::default()
        }
    }

    #[test]
    fn end_restores_the_previous_mode_and_clears_the_window() {
        for previous in [Some(SilentMode::Off), Some(SilentMode::Deny)] {
            let ended = end(settings_in_window(previous));
            assert_eq!(ended.silent_mode, previous);
            assert_eq!(ended.learning, None);
        }
    }

    #[test]
    fn end_restores_an_unchosen_mode_as_unchosen() {
        let ended = end(settings_in_window(None));
        assert_eq!(ended.silent_mode, None);
        assert_eq!(ended.effective_silent_mode(), SilentMode::Off);
        assert_eq!(ended.learning, None);
    }

    #[test]
    fn end_is_idempotent_and_keeps_other_fields() {
        let mut s = settings_in_window(Some(SilentMode::Deny));
        s.rdap_enabled = true;
        s.default_duration = "until_restart".into();
        let once = end(s);
        assert!(once.rdap_enabled);
        assert_eq!(once.default_duration, "until_restart");
        assert_eq!(end(once.clone()), once);
        // Ending without any window must not touch the mode.
        let plain = Settings {
            silent_mode: Some(SilentMode::Allow),
            ..Settings::default()
        };
        assert_eq!(end(plain.clone()), plain);
    }

    #[test]
    fn a_manual_change_ends_the_window_and_keeps_the_owners_mode() {
        let mut s = settings_in_window(Some(SilentMode::Off));
        s.silent_mode = Some(SilentMode::Deny);
        let ended = end_keeping_mode(s);
        assert_eq!(ended.silent_mode, Some(SilentMode::Deny));
        assert_eq!(ended.learning, None);
    }

    #[test]
    fn the_ticker_counts_only_forward_running_time() {
        let mut t = Ticker::default();
        assert_eq!(t.delta(5_000), 0, "the first reading only primes it");
        assert_eq!(t.delta(65_000), 60_000);
        assert_eq!(t.delta(65_000), 0);
        assert_eq!(t.delta(10_000), 0, "a backwards reading adds nothing");
        assert_eq!(t.delta(11_000), 1_000, "and counts on from the new reading");
    }

    #[test]
    fn clocks_can_be_injected_and_the_real_ones_move_forward() {
        let fake = Fake {
            wall: AtomicI64::new(7),
            mono: AtomicI64::new(9),
        };
        let clocks: &dyn Clocks = &fake;
        assert_eq!((clocks.wall_ms(), clocks.monotonic_ms()), (7, 9));
        let real = SystemClocks;
        assert!(
            real.wall_ms() > 1_700_000_000_000,
            "the wall clock is the epoch's"
        );
        let a = real.monotonic_ms();
        assert!(real.monotonic_ms() >= a);
    }
}
