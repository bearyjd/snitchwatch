//! A file a writer replaces by rename, opened just before: the reader's
//! handle is the old file with zero links. These tests do the replacement by
//! hand in the `after_open` hook, so they are deterministic.

use super::*;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub(crate) fn put(path: &Path, bytes: &[u8], mode: u32) {
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
pub(crate) fn replace(path: &Path, bytes: &[u8], mode: u32) {
    let temp = path.with_extension("new");
    put(&temp, bytes, mode);
    std::fs::rename(&temp, path).unwrap();
}

fn setup() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gui.log");
    put(&path, b"old", 0o600);
    (dir, path)
}

fn open_read(path: &Path) -> std::io::Result<File> {
    open_no_follow(path, std::fs::OpenOptions::new().read(true))
}

fn error_of<T>(r: std::io::Result<T>) -> String {
    r.map(drop).expect_err("an error").to_string()
}

#[test]
fn zero_links_then_a_valid_replacement_is_used() {
    let (_d, path) = setup();
    let mut calls = 0;
    let (file, meta) = open_settled(
        4,
        &mut || {
            calls += 1;
            if calls == 1 {
                replace(&path, b"new", 0o600);
            }
        },
        || open_read(&path),
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(file.metadata().unwrap().len(), 3);
    assert!(refuse_unless_ours_meta(&path, &meta).is_ok());
}

#[test]
fn zero_links_then_an_unsafe_replacement_is_refused_by_the_ownership_check() {
    // A FIFO in place of the file is not a regular file.
    let (_d, path) = setup();
    let mut first = true;
    let (_, meta) = open_settled(
        4,
        &mut || {
            if std::mem::take(&mut first) {
                std::fs::remove_file(&path).unwrap();
                let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: `c` is a valid NUL-terminated path.
                assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            }
        },
        || open_read(&path),
    )
    .unwrap();
    let error = error_of(refuse_unless_ours_meta(&path, &meta));
    assert!(error.contains("isn't a regular file"), "{error}");
}

#[test]
fn zero_links_then_a_hard_linked_replacement_is_refused() {
    let (d, path) = setup();
    let mut first = true;
    let (_, meta) = open_settled(
        4,
        &mut || {
            if std::mem::take(&mut first) {
                replace(&path, b"new", 0o600);
                std::fs::hard_link(&path, d.path().join("other")).unwrap();
            }
        },
        || open_read(&path),
    )
    .unwrap();
    let error = error_of(refuse_unless_ours_meta(&path, &meta));
    assert!(error.contains("hard link"), "{error}");
}

#[test]
fn zero_links_then_permanent_deletion_is_absent_not_an_error() {
    let (_d, path) = setup();
    let mut first = true;
    let error = open_settled(
        4,
        &mut || {
            if std::mem::take(&mut first) {
                std::fs::remove_file(&path).unwrap();
            }
        },
        || open_read(&path),
    )
    .expect_err("the file is gone");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    // `keep_previous` treats that as nothing to keep.
    let (_d, path) = setup();
    let mut first = true;
    keep_previous_with(&path, 4, &mut || {
        if std::mem::take(&mut first) {
            std::fs::remove_file(&path).unwrap();
        }
    })
    .unwrap();
}

#[test]
fn endless_replacement_gives_up_after_exactly_the_cap_with_a_distinct_error() {
    for run in [
        |path: &Path| keep_previous_with(path, 5, &mut || replace(path, b"x", 0o600)),
        |path: &Path| open_log_with(path, 5, &mut || replace(path, b"x", 0o600)).map(drop),
    ] {
        let (_d, path) = setup();
        let error = error_of(run(&path));
        assert!(error.contains(KEEPS_BEING_REPLACED), "{error}");
    }
    let (_d, path) = setup();
    let mut calls = 0;
    let error = error_of(open_settled(
        5,
        &mut || {
            calls += 1;
            replace(&path, b"x", 0o600);
        },
        || open_read(&path),
    ));
    assert_eq!(calls, 5);
    assert!(error.contains(KEEPS_BEING_REPLACED), "{error}");
}

#[test]
fn the_log_paths_use_a_valid_replacement() {
    let (_d, path) = setup();
    let mut first = true;
    let file = open_log_with(&path, 4, &mut || {
        if std::mem::take(&mut first) {
            replace(&path, b"new", 0o600);
        }
    })
    .unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
}

/// A hook for the window after the accepting `fstat`.
pub(crate) struct OnCheck<F>(pub(crate) F);

impl<F: FnMut()> Hooks for OnCheck<F> {
    fn after_check(&mut self) {
        (self.0)();
    }
}

#[test]
fn a_rename_after_the_accepting_fstat_is_not_a_hard_link() {
    // The caller validates from the snapshot, never a second fstat (which
    // would now see zero links).
    let (_d, path) = setup();
    keep_previous_with(&path, 4, &mut OnCheck(|| replace(&path, b"x", 0o600))).unwrap();
    let (_d, path) = setup();
    open_log_with(&path, 4, &mut OnCheck(|| replace(&path, b"x", 0o600))).unwrap();
}
