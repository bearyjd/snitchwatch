//! PR 10 repair (L-a): a bridge that takes the writer lock lazily resumes
//! recording when history is enabled, and never owns a directory that holds
//! no history yet.

use super::control::{ControlGate, Limits, Refused};
use super::control_support::consent;
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, Mode};

#[test]
fn a_lazy_lock_resumes_recording_on_enabled_history() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    // This bridge starts before the directory exists; another one enables.
    let late = ControlGate::start(Mode::User, Some(&state), Limits::default());
    assert!(late.slot().get().is_none());
    enable(&location, consent()).unwrap();
    late.preview().unwrap();
    assert!(late.slot().get().is_some(), "the lazy owner records");
}

#[test]
fn a_preview_in_a_directory_with_no_history_takes_no_lock() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let other = ControlGate::start(Mode::User, Some(&state), Limits::default());
    let enabling = ControlGate::start(Mode::User, Some(&state), Limits::default());
    // The enabling bridge has made the directory and not yet taken its lock.
    super::open::ensure_dir(location.dir(), super::open::effective_uid()).unwrap();
    let result = other.preview();
    assert!(matches!(result, Err(Refused::Unavailable(_))), "{result:?}");
    enabling.enable(consent()).unwrap();
    assert!(enabling.slot().get().is_some(), "the enabler records");
}
