//! The settings transaction against real files in temporary directories. A
//! write failure is injected the way `settings.rs`' own tests do it: a
//! symlinked settings file is readable but `write_settings` refuses it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use crate::settings::{Learning, LEARNING_DEFAULT_MS};

const START: i64 = 5_000_000;
const AFTER: i64 = START + LEARNING_DEFAULT_MS + 1;

struct Fake {
    wall: AtomicI64,
    mono: AtomicI64,
}

impl Fake {
    fn at(wall: i64, mono: i64) -> Self {
        Self {
            wall: AtomicI64::new(wall),
            mono: AtomicI64::new(mono),
        }
    }
    fn set(&self, wall: i64, mono: i64) {
        self.wall.store(wall, Ordering::SeqCst);
        self.mono.store(mono, Ordering::SeqCst);
    }
}

impl Clocks for Fake {
    fn wall_ms(&self) -> i64 {
        self.wall.load(Ordering::SeqCst)
    }
    fn monotonic_ms(&self) -> i64 {
        self.mono.load(Ordering::SeqCst)
    }
}

/// A settings file at `path()`; `read_only()` turns it into a symlink that can
/// be read but not written, `writable()` puts a regular file back.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self) -> PathBuf {
        self.dir.path().join("settings.json")
    }
    fn real(&self) -> PathBuf {
        self.dir.path().join("real.json")
    }
    fn seed(&self, settings: &Settings) {
        write_settings(&self.real(), settings).unwrap();
        std::fs::copy(self.real(), self.path()).unwrap();
    }
    fn read_only(&self) {
        std::fs::remove_file(self.path()).unwrap();
        std::os::unix::fs::symlink(self.real(), self.path()).unwrap();
    }
    fn writable(&self) {
        std::fs::remove_file(self.path()).unwrap();
        std::fs::copy(self.real(), self.path()).unwrap();
    }
    fn disk(&self) -> Settings {
        read_settings(&self.path())
    }
}

fn window(previous: Option<SilentMode>) -> Learning {
    Learning::begin(START, LEARNING_DEFAULT_MS, previous)
}

fn learning_settings(previous: Option<SilentMode>) -> Settings {
    Settings {
        silent_mode: Some(SilentMode::Allow),
        learning: Some(window(previous)),
        ..Settings::default()
    }
}

fn unrelated(settings: &mut Settings) {
    settings.default_duration = "until_restart".into();
}

fn ticker() -> Mutex<Ticker> {
    Mutex::new(Ticker::default())
}

// --- the three end-check points -------------------------------------------

#[test]
fn startup_ends_an_expired_window_before_the_mode_is_applied() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    let store = SettingsStore::new();
    let applied = startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(applied.learning, None);
    assert_eq!(fx.disk(), applied, "the restore reached the disk");
    assert!(!store.write_failed());
}

#[test]
fn startup_restores_an_unchosen_mode_as_unchosen() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(None));
    let applied = startup(
        &SettingsStore::new(),
        &fx.path(),
        &Fake::at(AFTER, 0),
        &ticker(),
    )
    .settings;
    assert_eq!(applied.silent_mode, None);
    assert_eq!(fx.disk().silent_mode, None);
}

#[test]
fn startup_leaves_a_live_window_and_the_file_alone() {
    let fx = Fixture::new();
    let seeded = learning_settings(Some(SilentMode::Off));
    fx.seed(&seeded);
    fx.read_only(); // a write attempt would be noticed
    let store = SettingsStore::new();
    let applied = startup(&store, &fx.path(), &Fake::at(START + 1, 0), &ticker()).settings;
    assert_eq!(applied, seeded);
    assert!(!store.write_failed());
}

#[test]
fn an_unrelated_write_ends_an_expired_window_instead_of_reapplying_allow() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let applied = store.transact(&fx.path(), AFTER, None, unrelated).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Off));
    assert_eq!(applied.learning, None);
    assert_eq!(
        applied.default_duration, "until_restart",
        "the change itself landed"
    );
    assert_eq!(fx.disk(), applied);
}

#[test]
fn an_unrelated_write_keeps_a_live_window() {
    let fx = Fixture::new();
    let seeded = learning_settings(Some(SilentMode::Off));
    fx.seed(&seeded);
    let applied = SettingsStore::new()
        .transact(&fx.path(), START + 10, None, unrelated)
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Allow));
    assert_eq!(applied.learning, seeded.learning);
}

#[test]
fn the_expiry_timer_saves_running_time_and_ends_the_window_at_the_bound() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    let store = SettingsStore::new();
    let (clocks, tk) = (Fake::at(START + 1_000, 100), ticker());
    // Startup primes the ticker; nothing is counted yet.
    assert!(startup(&store, &fx.path(), &clocks, &tk)
        .settings
        .learning
        .is_some());
    clocks.set(START + 61_000, 60_100);
    let after_a_minute = tick(&store, &fx.path(), &clocks, &tk).settings;
    assert_eq!(after_a_minute.learning.unwrap().elapsed_ms, 60_000);
    assert_eq!(fx.disk().learning.unwrap().elapsed_ms, 60_000, "saved");
    // The wall clock is fine but the running time reaches the duration.
    clocks.set(START + 62_000, 60_100 + LEARNING_DEFAULT_MS);
    let ended = tick(&store, &fx.path(), &clocks, &tk).settings;
    assert_eq!(ended.silent_mode, Some(SilentMode::Deny));
    assert_eq!(ended.learning, None);
    assert_eq!(fx.disk(), ended);
}

#[test]
fn the_expiry_timer_ends_the_window_when_the_wall_clock_passes_its_end() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let (clocks, tk) = (Fake::at(START, 0), ticker());
    let store = SettingsStore::new();
    startup(&store, &fx.path(), &clocks, &tk);
    clocks.set(AFTER, 1_000); // asleep for a day: the monotonic clock barely moved
    let ended = tick(&store, &fx.path(), &clocks, &tk).settings;
    assert_eq!(ended.silent_mode, Some(SilentMode::Off));
    assert_eq!(fx.disk().learning, None);
}

#[test]
fn the_expiry_timer_writes_nothing_without_a_window() {
    let fx = Fixture::new();
    let clocks = Fake::at(START, 0);
    let tk = ticker();
    let store = SettingsStore::new();
    tick(&store, &fx.path(), &clocks, &tk);
    clocks.set(START + 60_000, 60_000);
    tick(&store, &fx.path(), &clocks, &tk);
    assert!(!fx.path().exists(), "no churn when nothing changed");
}

// --- fail closed -----------------------------------------------------------

#[test]
fn a_failed_restore_write_does_not_leave_allow_applied() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    fx.read_only();
    let store = SettingsStore::new();
    let applied = startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(applied.learning, None);
    assert!(store.write_failed(), "the notice is up");
    assert_eq!(
        fx.disk().silent_mode,
        Some(SilentMode::Allow),
        "disk unchanged"
    );
    // The silent-state push cannot bring Allow back either.
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Deny);
    assert_eq!(store.honor(fx.disk()).silent_mode, Some(SilentMode::Deny));
}

#[test]
fn a_failed_restore_of_an_unchosen_mode_applies_unchosen() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(None));
    fx.read_only();
    let store = SettingsStore::new();
    let applied = startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, None);
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Off);
}

#[test]
fn a_restore_to_allow_that_cannot_be_written_is_still_applied_as_off() {
    // previous_mode Allow: the owner had Allow before the window. Allow is
    // never applied while the override holds.
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Allow)));
    fx.read_only();
    let store = SettingsStore::new();
    let applied = startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Off));
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Off);
    // The next write that works persists the real intent (Allow).
    fx.writable();
    let later = store.transact(&fx.path(), AFTER, None, unrelated).settings;
    assert_eq!(later.silent_mode, Some(SilentMode::Allow));
    assert!(!store.write_failed());
}

#[test]
fn a_failed_explicit_deny_stays_deny_never_the_old_mode() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    fx.read_only();
    let store = SettingsStore::new();
    let applied = store
        .transact(&fx.path(), START + 5, Some(SilentMode::Deny), |_| {})
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(applied.learning, None);
    assert!(store.write_failed());
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Deny);
}

#[test]
fn an_explicit_choice_replaces_a_held_restore() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    fx.read_only();
    let store = SettingsStore::new();
    startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker());
    let applied = store
        .transact(&fx.path(), AFTER, Some(SilentMode::Off), |_| {})
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Off));
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Off);
}

#[test]
fn an_unrelated_successful_write_reapplies_the_intent_and_clears_the_override() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    fx.read_only();
    let store = SettingsStore::new();
    store.transact(&fx.path(), START + 5, Some(SilentMode::Deny), |_| {});
    assert!(store.write_failed());
    fx.writable();
    // The disk still says Allow plus a live window; this write is unrelated.
    assert_eq!(fx.disk().silent_mode, Some(SilentMode::Allow));
    let applied = store
        .transact(&fx.path(), START + 6, None, unrelated)
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(fx.disk().silent_mode, Some(SilentMode::Deny));
    assert_eq!(
        fx.disk().learning,
        None,
        "no allow plus an active window on disk"
    );
    assert_eq!(fx.disk().default_duration, "until_restart");
    assert!(!store.write_failed(), "cleared by the write that succeeded");
}

#[test]
fn a_failed_write_does_not_clear_the_override() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    fx.read_only();
    let store = SettingsStore::new();
    startup(&store, &fx.path(), &Fake::at(AFTER, 0), &ticker());
    for _ in 0..2 {
        let applied = store.transact(&fx.path(), AFTER, None, unrelated).settings;
        assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
        assert!(store.write_failed(), "still held after a failed write");
    }
}

#[test]
fn a_failed_unrelated_write_without_an_intent_holds_nothing() {
    let fx = Fixture::new();
    fx.seed(&Settings::default());
    fx.read_only();
    let store = SettingsStore::new();
    store.transact(&fx.path(), START, None, unrelated);
    assert!(!store.write_failed());
}

// --- restart ---------------------------------------------------------------

#[test]
fn a_restart_after_a_failed_restore_write_still_applies_no_allow() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    fx.read_only();
    startup(
        &SettingsStore::new(),
        &fx.path(),
        &Fake::at(AFTER, 0),
        &ticker(),
    );
    // New process: the override is gone, the disk still says Allow.
    let restarted = SettingsStore::new();
    let applied = startup(&restarted, &fx.path(), &Fake::at(AFTER + 9, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert!(restarted.write_failed());
}

#[test]
fn a_failed_explicit_deny_then_a_good_write_survives_a_restart_as_deny() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    fx.read_only();
    let store = SettingsStore::new();
    store.transact(&fx.path(), START + 5, Some(SilentMode::Deny), |_| {});
    fx.writable();
    store.transact(&fx.path(), START + 6, None, unrelated);
    let restarted = SettingsStore::new();
    let applied = startup(&restarted, &fx.path(), &Fake::at(START + 7, 0), &ticker()).settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(applied.learning, None);
}

// --- manual change ---------------------------------------------------------

#[test]
fn a_manual_silent_mode_change_ends_learning_and_keeps_the_owners_mode() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let applied = store
        .transact(&fx.path(), START + 5, Some(SilentMode::Deny), |s| {
            s.silent_mode = Some(SilentMode::Deny);
        })
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Deny));
    assert_eq!(fx.disk().silent_mode, Some(SilentMode::Deny));
    assert_eq!(fx.disk().learning, None);
    assert!(!store.write_failed());
}

#[test]
fn choosing_allow_by_hand_during_a_window_ends_the_window_and_keeps_allow() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    let store = SettingsStore::new();
    let applied = store
        .transact(&fx.path(), START + 5, Some(SilentMode::Allow), |_| {})
        .settings;
    assert_eq!(applied.silent_mode, Some(SilentMode::Allow));
    assert_eq!(fx.disk().learning, None, "a plain Allow, not learning");
}

#[test]
fn without_an_override_honor_changes_nothing() {
    let store = SettingsStore::new();
    let s = learning_settings(None);
    assert_eq!(store.honor(s.clone()), s);
    assert_eq!(store.honor_mode(SilentMode::Allow), SilentMode::Allow);
    assert!(!store.write_failed());
}

// --- running time, numbering, demo, known limit ------------------------------

#[test]
fn a_second_startup_does_not_discard_the_running_time_counted_so_far() {
    // `refreshPreferences` runs again on every Diagnostics page load.
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let (clocks, tk) = (Fake::at(START, 100), ticker());
    startup(&store, &fx.path(), &clocks, &tk);
    clocks.set(START + 30_000, 30_100);
    startup(&store, &fx.path(), &clocks, &tk); // a page load half-way
    clocks.set(START + 60_000, 60_100);
    let ticked = tick(&store, &fx.path(), &clocks, &tk).settings;
    assert_eq!(ticked.learning.unwrap().elapsed_ms, 60_000);
}

#[test]
fn every_transaction_is_numbered_in_order() {
    let fx = Fixture::new();
    fx.seed(&Settings::default());
    let store = SettingsStore::new();
    let first = store.transact(&fx.path(), START, None, |_| {});
    let second = store.transact(&fx.path(), START, None, unrelated);
    let third = store.transact(&fx.path(), START, None, |_| {});
    assert!(first.generation < second.generation && second.generation < third.generation);
}

#[test]
fn a_stale_snapshot_is_not_applied_after_a_newer_one() {
    // A tick thread snapshots a live Allow; a chooser Deny then commits and
    // queues first. The older snapshot must not be applied afterwards.
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let (clocks, tk) = (Fake::at(START + 1_000, 0), ticker());
    let stale = tick(&store, &fx.path(), &clocks, &tk);
    assert_eq!(stale.settings.silent_mode, Some(SilentMode::Allow));
    let newer = store.transact(&fx.path(), START + 2_000, Some(SilentMode::Deny), |_| {});
    assert!(store.claim(newer.generation, false));
    assert!(
        !store.claim(stale.generation, false),
        "the stale Allow is skipped"
    );
    assert!(
        !store.claim(newer.generation, false),
        "and nothing is applied twice"
    );
    let latest = store.transact(&fx.path(), START + 3_000, None, unrelated);
    assert!(store.claim(latest.generation, false));
}

#[test]
fn snapshots_applied_in_order_are_all_applied() {
    let fx = Fixture::new();
    fx.seed(&Settings::default());
    let store = SettingsStore::new();
    let a = store.transact(&fx.path(), START, None, |_| {});
    let b = store.transact(&fx.path(), START, None, unrelated);
    assert!(store.claim(a.generation, false) && store.claim(b.generation, false));
}

#[test]
fn demo_mode_never_applies_the_real_files_snapshot() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let snapshot = store.transact(&fx.path(), START, None, unrelated);
    assert!(!store.claim(snapshot.generation, true));
    assert!(
        store.claim(snapshot.generation, false),
        "demo did not use it up"
    );
}

#[test]
fn a_failing_write_is_logged_once_and_recovery_once() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Deny)));
    fx.read_only();
    let store = SettingsStore::new();
    assert!(store.last_write_ok.load(Ordering::SeqCst));
    for _ in 0..3 {
        store.transact(&fx.path(), AFTER, None, unrelated);
        assert!(!store.last_write_ok.load(Ordering::SeqCst));
    }
    fx.writable();
    store.transact(&fx.path(), AFTER, None, unrelated);
    assert!(store.last_write_ok.load(Ordering::SeqCst));
}

/// KNOWN LIMIT, pinned on purpose (see the module doc): the override is in
/// memory only. A failed explicit Deny while the window is still live, with no
/// working write afterwards, comes back as Allow plus the live window after a
/// restart. If this test has to change, the limit changed: update the module
/// doc and the plan's Honest limits too.
#[test]
fn known_limit_a_restart_after_a_failed_explicit_deny_with_a_live_window_brings_allow_back() {
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    fx.read_only();
    let first = SettingsStore::new();
    let denied = first.transact(&fx.path(), START + 5, Some(SilentMode::Deny), |_| {});
    assert_eq!(denied.settings.silent_mode, Some(SilentMode::Deny));

    let restarted = SettingsStore::new();
    let applied = startup(&restarted, &fx.path(), &Fake::at(START + 10, 0), &ticker()).settings;
    assert_eq!(
        applied.silent_mode,
        Some(SilentMode::Allow),
        "the known limit"
    );
    assert!(
        applied.learning.is_some(),
        "the known limit: the window is live again"
    );
}

#[test]
fn known_limit_does_not_reach_an_expired_window() {
    // The same restart once the window is over: Allow is never applied.
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    fx.read_only();
    SettingsStore::new().transact(&fx.path(), START + 5, Some(SilentMode::Deny), |_| {});
    let restarted = SettingsStore::new();
    let applied = startup(&restarted, &fx.path(), &Fake::at(AFTER, 0), &ticker()).settings;
    assert_ne!(applied.silent_mode, Some(SilentMode::Allow));
    assert_eq!(applied.learning, None);
}

#[test]
fn a_stale_rdap_toggle_still_sets_the_rdap_property_but_not_the_mode() {
    // The toggle commits first; a tick commits after it and is applied first.
    // The toggle's snapshot is stale for the mode but its RDAP value is not.
    let fx = Fixture::new();
    fx.seed(&learning_settings(Some(SilentMode::Off)));
    let store = SettingsStore::new();
    let toggle = store.transact(&fx.path(), START + 1, None, |s| s.rdap_enabled = true);
    let (clocks, tk) = (Fake::at(AFTER, 0), ticker());
    let tick_after = tick(&store, &fx.path(), &clocks, &tk); // ends the window
    assert_eq!(tick_after.settings.silent_mode, Some(SilentMode::Off));
    assert!(store
        .toggle_outcome(&tick_after, false)
        .preferences
        .is_some());

    let late = store.toggle_outcome(&toggle, false);
    assert!(late.rdap_enabled, "the toggle's value is not mode ordering");
    assert!(
        late.preferences.is_none(),
        "the older mode snapshot is not applied"
    );
    assert_eq!(
        toggle.settings.silent_mode,
        Some(SilentMode::Allow),
        "it was stale"
    );
}

#[test]
fn the_rdap_toggle_in_demo_mode_sets_the_property_and_applies_nothing_else() {
    let fx = Fixture::new();
    fx.seed(&Settings::default());
    let store = SettingsStore::new();
    let toggle = store.transact(&fx.path(), START, None, |s| s.rdap_enabled = true);
    let outcome = store.toggle_outcome(&toggle, true);
    assert!(outcome.rdap_enabled);
    assert!(outcome.preferences.is_none());
}
