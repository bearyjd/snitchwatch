//! Test helpers: tempdirs under the worktree's own (git-ignored) target
//! directory, not `/tmp` (the run plan's rule).

use std::path::PathBuf;

pub(crate) fn tempdir() -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/history-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("h-")
        .tempdir_in(root)
        .unwrap()
}

/// A canonical, absolute state directory inside a fresh tempdir.
pub(crate) fn state_dir(root: &tempfile::TempDir) -> PathBuf {
    let dir = root.path().canonicalize().unwrap().join("state");
    std::fs::create_dir(&dir).unwrap();
    dir
}
