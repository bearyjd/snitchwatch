//! "Start learning" (phase L4b) and the ordering gate between a stale
//! "recording stops" and a newly applied start.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use super::*;
use crate::settings::LEARNING_DEFAULT_MS;

const NOW: i64 = 9_000_000;

struct Clock(AtomicI64);

impl Clock {
    fn at(ms: i64) -> Self {
        Self(AtomicI64::new(ms))
    }
}

impl Clocks for Clock {
    fn wall_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn file_with(settings: &Settings) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    write_settings(&path, settings).unwrap();
    (dir, path)
}

#[test]
fn start_begins_a_window_with_allow_and_keeps_the_mode_it_replaced() {
    for previous in [None, Some(SilentMode::Off), Some(SilentMode::Deny)] {
        let (_dir, path) = file_with(&Settings {
            silent_mode: previous,
            ..Settings::default()
        });
        let store = SettingsStore::new();
        let applied = start(&store, &path, &Clock::at(NOW), LEARNING_DEFAULT_MS).unwrap();
        let on_disk = read_settings(&path);
        assert_eq!(applied.settings, on_disk);
        assert_eq!(on_disk.silent_mode, Some(SilentMode::Allow));
        let window = on_disk.learning.clone().expect("a window");
        assert_eq!(window.previous_mode, previous, "{previous:?}");
        assert_eq!(window.started_ms, NOW);
        assert_eq!(window.ends_ms, NOW + LEARNING_DEFAULT_MS);
        assert!(store.learning_in_force(&on_disk).is_some());
    }
}

#[test]
fn start_refuses_a_length_outside_zero_to_seven_days() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    for bad in [0, -1, LEARNING_MAX_MS + 1, i64::MAX] {
        assert_eq!(
            start(&store, &path, &Clock::at(NOW), bad).unwrap_err(),
            StartRefusal::BadDuration,
            "{bad}"
        );
    }
    assert_eq!(read_settings(&path).learning, None);
    assert!(start(&store, &path, &Clock::at(NOW), LEARNING_MAX_MS).is_ok());
}

#[test]
fn a_running_window_is_never_extended_or_replaced() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    start(&store, &path, &Clock::at(NOW), 60_000).unwrap();
    let before = read_settings(&path);
    assert_eq!(
        start(&store, &path, &Clock::at(NOW + 1_000), LEARNING_MAX_MS).unwrap_err(),
        StartRefusal::Running
    );
    assert_eq!(read_settings(&path), before);
}

#[test]
fn a_window_that_is_over_is_ended_and_a_new_one_begins_with_the_old_mode_kept() {
    let (_dir, path) = file_with(&Settings {
        silent_mode: Some(SilentMode::Deny),
        ..Settings::default()
    });
    let store = SettingsStore::new();
    start(&store, &path, &Clock::at(NOW), 60_000).unwrap();
    start(&store, &path, &Clock::at(NOW + 120_000), 60_000).unwrap();
    let window = read_settings(&path).learning.expect("a new window");
    assert_eq!(window.started_ms, NOW + 120_000);
    assert_eq!(
        window.previous_mode,
        Some(SilentMode::Deny),
        "the owner's own mode, not Allow"
    );
}

#[test]
fn start_does_not_start_while_the_settings_file_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    let path = dir.path().join("settings.json");
    write_settings(&real, &Settings::default()).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let store = SettingsStore::new();
    // A write that fails sets the override ...
    store.transact(&path, NOW, Some(SilentMode::Deny), |_| {});
    assert!(store.write_failed());
    // ... and then a start would be wiped by it, so it is refused.
    assert_eq!(
        start(&store, &path, &Clock::at(NOW), 60_000).unwrap_err(),
        StartRefusal::WriteFailed
    );
}

#[test]
fn a_started_window_survives_an_unrelated_write_startup_and_a_tick() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    let clock = Clock::at(NOW);
    start(&store, &path, &clock, 3_600_000).unwrap();
    store.transact(&path, NOW + 1, None, |s| {
        s.default_duration = "until_restart".into()
    });
    let ticker = Mutex::new(Ticker::default());
    startup(&store, &path, &clock, &ticker);
    let after = tick(&store, &path, &Clock::at(NOW + 10), &ticker).settings;
    assert!(after.learning.is_some());
    assert_eq!(after.silent_mode, Some(SilentMode::Allow));
}

#[test]
fn a_manual_mode_choice_still_ends_a_started_window() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    start(&store, &path, &Clock::at(NOW), 3_600_000).unwrap();
    let applied = store.transact(&path, NOW + 1, Some(SilentMode::Deny), |s| {
        s.silent_mode = Some(SilentMode::Deny);
        s.learning = None;
    });
    assert_eq!(applied.settings.learning, None);
    assert_eq!(applied.settings.silent_mode, Some(SilentMode::Deny));
}

// --- the ordering gate ------------------------------------------------------

#[test]
fn a_stale_end_does_not_stop_recording_after_a_newer_snapshot() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    let ended = stop(&store, &path, &Clock::at(NOW)); // no window: learning None
    start(&store, &path, &Clock::at(NOW + 1), 60_000).unwrap();
    let ran = AtomicBool::new(false);
    assert!(!store.end_learning_if_latest(&ended, || ran.store(true, Ordering::SeqCst)));
    assert!(!ran.load(Ordering::SeqCst), "a newer transaction exists");
}

#[test]
fn the_newest_snapshot_that_ends_the_window_stops_recording() {
    let (_dir, path) = file_with(&Settings::default());
    let store = SettingsStore::new();
    start(&store, &path, &Clock::at(NOW), 60_000).unwrap();
    let ended = stop(&store, &path, &Clock::at(NOW + 5));
    let ran = AtomicBool::new(false);
    assert!(store.end_learning_if_latest(&ended, || ran.store(true, Ordering::SeqCst)));
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn recording_is_not_stopped_while_a_snapshot_is_being_applied() {
    let (_dir, path) = file_with(&Settings::default());
    let store = Arc::new(SettingsStore::new());
    start(&store, &path, &Clock::at(NOW), 60_000).unwrap();
    let ended = stop(&store, &path, &Clock::at(NOW + 5));

    // The Qt thread is applying a snapshot (holding the gate).
    let applying = store.gate();
    let (about_to_end, started) = mpsc::channel();
    let ran = Arc::new(AtomicBool::new(false));
    let worker = {
        let (store, ran) = (store.clone(), ran.clone());
        std::thread::spawn(move || {
            about_to_end.send(()).unwrap();
            store.end_learning_if_latest(&ended, || ran.store(true, Ordering::SeqCst))
        })
    };
    started.recv().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !ran.load(Ordering::SeqCst),
        "recording was switched off in the middle of applying a snapshot"
    );
    drop(applying);
    assert!(worker.join().unwrap());
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn a_start_whose_write_fails_is_not_reported_as_started() {
    // Readable but not writable, and no override set beforehand: the
    // pre-check passes, the write inside the transaction fails.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    let path = dir.path().join("settings.json");
    write_settings(&real, &Settings::default()).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let store = SettingsStore::new();
    assert!(!store.write_failed());
    assert_eq!(
        start(&store, &path, &Clock::at(NOW), 60_000).unwrap_err(),
        StartRefusal::WriteFailed
    );
    assert_eq!(read_settings(&path).learning, None);
}

#[test]
fn a_failed_start_over_an_old_window_is_not_mistaken_for_the_new_one() {
    // An over-but-unended window sits in a file that cannot be written: the
    // re-read still shows a window, but it is not the one just asked for.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    let path = dir.path().join("settings.json");
    write_settings(&real, &Settings::default()).unwrap();
    {
        let store = SettingsStore::new();
        std::fs::copy(&real, &path).unwrap();
        start(&store, &path, &Clock::at(NOW), 60_000).unwrap();
    }
    std::fs::copy(&path, &real).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let store = SettingsStore::new();
    assert_eq!(
        start(&store, &path, &Clock::at(NOW + 120_000), 60_000).unwrap_err(),
        StartRefusal::WriteFailed
    );
}
