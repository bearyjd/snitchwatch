//! A reader that opens a file a save then renames over sees zero links. These
//! tests drive the replacement by hand, through the read hooks, so they are
//! deterministic: no threads, no timing.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::settings::read_checked_with;
use super::test_support::tempdir;
use crate::state_file::read_with;

const KEEPS_BEING_REPLACED: &str = "keeps being replaced";

fn put(path: &Path, bytes: &[u8], mode: u32) {
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut f, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// What a save does: write a new file beside, rename it over `path`.
fn replace(path: &Path, bytes: &[u8], mode: u32) {
    let temp = path.with_extension("new");
    put(&temp, bytes, mode);
    std::fs::rename(&temp, path).unwrap();
}

fn setup(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir();
    let path = dir.path().join("f.json");
    put(&path, bytes, 0o600);
    (dir, path)
}

fn message<T, E: std::fmt::Display>(r: Result<T, E>) -> String {
    r.map(drop)
        .map_err(|e| e.to_string())
        .expect_err("an error")
}

#[test]
fn zero_links_then_a_valid_replacement_is_read() {
    let (_d, path) = setup(b"old");
    let mut calls = 0;
    let read = read_with(&path, 4096, 4, &mut || {
        calls += 1;
        if calls == 1 {
            replace(&path, b"new", 0o600);
        }
    });
    assert_eq!(read.unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(calls, 2);
}

#[test]
fn zero_links_then_an_unsafe_replacement_is_refused() {
    // Group/other-writable.
    let (_d, path) = setup(b"old");
    let mut first = true;
    let error = message(read_with(&path, 4096, 4, &mut || {
        if std::mem::take(&mut first) {
            replace(&path, b"new", 0o666);
        }
    }));
    assert!(!error.contains(KEEPS_BEING_REPLACED), "{error}");
    // A second name.
    let (d, path) = setup(b"old");
    let mut first = true;
    let error = message(read_with(&path, 4096, 4, &mut || {
        if std::mem::take(&mut first) {
            replace(&path, b"new", 0o600);
            std::fs::hard_link(&path, d.path().join("other")).unwrap();
        }
    }));
    assert!(error.contains("hard link"), "{error}");
}

#[test]
fn zero_links_then_a_hard_linked_replacement_is_refused_for_settings_too() {
    let (d, path) = setup(b"{}");
    let mut first = true;
    let error = message(read_checked_with(&path, 4, &mut || {
        if std::mem::take(&mut first) {
            replace(&path, b"{}", 0o600);
            std::fs::hard_link(&path, d.path().join("other")).unwrap();
        }
    }));
    assert!(error.contains("hard link"), "{error}");
}

#[test]
fn zero_links_then_a_valid_replacement_is_read_for_settings_too() {
    let (_d, path) = setup(b"old");
    let mut calls = 0;
    let read = read_checked_with(&path, 4, &mut || {
        calls += 1;
        if calls == 1 {
            replace(&path, b"new", 0o600);
        }
    });
    assert_eq!(read.unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn zero_links_then_permanent_deletion_is_absent_not_an_error() {
    let (_d, path) = setup(b"old");
    let read = read_with(&path, 4096, 4, &mut || {
        let _ = std::fs::remove_file(&path);
    });
    assert_eq!(read.unwrap(), None);
    let (_d, path) = setup(b"old");
    let read = read_checked_with(&path, 4, &mut || {
        let _ = std::fs::remove_file(&path);
    });
    assert_eq!(read.unwrap(), None);
}

#[test]
fn endless_replacement_fails_closed_with_a_distinct_error() {
    let (_d, path) = setup(b"old");
    let mut calls = 0;
    let error = message(read_with(&path, 4096, 3, &mut || {
        calls += 1;
        replace(&path, b"new", 0o600);
    }));
    assert!(error.contains(KEEPS_BEING_REPLACED), "{error}");
    assert_eq!(calls, 3, "exactly the cap");
    let (_d, path) = setup(b"old");
    let error = message(read_checked_with(&path, 3, &mut || {
        replace(&path, b"new", 0o600);
    }));
    assert!(error.contains(KEEPS_BEING_REPLACED), "{error}");
}
