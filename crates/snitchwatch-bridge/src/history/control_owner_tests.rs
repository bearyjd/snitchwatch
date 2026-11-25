//! PR 10 review (H1, L1): only the bridge that holds the writer lock may
//! preview, clear, delete or disable, including a bridge that started before
//! the history directory existed.

use std::fs;

use super::control::{ControlGate, Limits, Refused};
use super::control_preview::TokenId;
use super::control_support::{consent, count, event};
use super::test_support::{state_dir, tempdir};
use super::{locate, Mode};

fn in_use(result: Result<(), Refused>, what: &str) {
    match result {
        Err(Refused::Unavailable(reason)) => {
            assert!(reason.contains("another bridge"), "{what}: {reason}")
        }
        other => panic!("{what}: expected a refusal for the lock, got {other:?}"),
    }
}

#[test]
fn a_bridge_that_started_before_the_directory_existed_cannot_touch_the_owners_history() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let one = ControlGate::start(Mode::User, Some(&state), Limits::default());
    let two = ControlGate::start(Mode::User, Some(&state), Limits::default());
    one.enable(consent()).unwrap();
    let capture = one.slot().get().expect("recording");
    capture.observe(&[&event("/usr/bin/curl", "owned.example", 20)]);
    let rows = "SELECT COUNT(*) FROM events";
    super::control_tests::wait_for("the row", || count(&location, rows) == 1);
    let settings = fs::read_to_string(location.settings_path()).unwrap();
    let names = |dir: &std::path::Path| {
        let mut n: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        n.sort();
        n
    };
    let files = names(location.dir());

    assert!(
        matches!(two.preview(), Err(Refused::Unavailable(ref r)) if r.contains("another bridge"))
    );
    in_use(two.clear(TokenId(1)), "clear");
    in_use(two.delete(TokenId(1)), "delete");
    in_use(two.disable(), "disable");

    assert_eq!(
        fs::read_to_string(location.settings_path()).unwrap(),
        settings
    );
    assert_eq!(names(location.dir()), files, "no file renamed or removed");
    assert_eq!(count(&location, rows), 1);
    capture.observe(&[&event("/usr/bin/curl", "still.example", 21)]);
    super::control_tests::wait_for("it still records", || count(&location, rows) == 2);
    assert!(one.slot().get().is_some());
}

#[test]
fn history_never_enabled_has_nothing_to_preview_clear_delete_or_disable() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
    for result in [
        gate.preview().map(|_| ()),
        gate.clear(TokenId(1)),
        gate.delete(TokenId(1)),
        gate.disable(),
    ] {
        assert!(
            matches!(result, Err(Refused::Unavailable(ref r)) if r.contains("never been enabled")),
            "{result:?}"
        );
    }
    assert!(!location.dir().exists(), "nothing was created");
}

#[test]
fn the_first_enable_finishes_a_leftover_removal_under_its_new_lock() {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    let gate = ControlGate::start(Mode::User, Some(&state), Limits::default());
    // A crashed removal's leftover appears after this bridge started.
    super::open::ensure_dir(location.dir(), super::open::effective_uid()).unwrap();
    let tombstone = location.dir().join("history.db.deleting");
    fs::write(&tombstone, "half-removed").unwrap();
    gate.enable(consent()).unwrap();
    assert!(
        !tombstone.exists(),
        "finished once the lock was first taken"
    );
    assert!(gate.slot().get().is_some());
}
