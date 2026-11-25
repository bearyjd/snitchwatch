//! The reader's look at the settings file is strictly read-only: no chmod,
//! no rewrite, whether history is on or off.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::reader::*;
use super::reader_fixture::Fx;

fn stamp(path: &std::path::Path) -> (u32, i64, i64, u64) {
    let m = std::fs::metadata(path).unwrap();
    (m.mode() & 0o7777, m.mtime(), m.mtime_nsec(), m.ino())
}

fn settings_stays_untouched(fx: &Fx) -> Outcome {
    let path = fx.rig.location.settings_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let before = stamp(&path);
    let outcome = fx.read(None, 10);
    assert_eq!(stamp(&path), before, "the reader touched the settings file");
    outcome
}

#[test]
fn reading_a_0400_settings_file_does_not_change_its_mode_or_mtime() {
    let fx = Fx::new();
    fx.events(1..=5);
    assert!(matches!(settings_stays_untouched(&fx), Outcome::Page(_)));
}

#[test]
fn a_disabled_history_is_answered_without_touching_the_settings_file() {
    let fx = Fx::new();
    let mut settings = super::settings::load(&fx.rig.location).unwrap().unwrap();
    settings.enabled = false;
    super::settings::save(&fx.rig.location, &settings).unwrap();
    match settings_stays_untouched(&fx) {
        Outcome::Unavailable(u) => assert_eq!(u.kind, UnavailKind::Off),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_settings_file_others_can_read_is_served_but_never_repaired_by_the_reader() {
    let fx = Fx::new();
    let path = fx.rig.location.settings_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let outcome = fx.read(None, 10);
    assert!(matches!(outcome, Outcome::Page(_)), "{outcome:?}");
    assert_eq!(stamp(&path).0, 0o644, "the reader repaired the mode");
}
