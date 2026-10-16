//! Owner-only files and directories under the state directory (#116).
//!
//! `gui.log` got this first (`crate::logging`); `crash.log` and the state
//! directory itself use the same pattern: a directory that is ours, 0700
//! (an existing one is tightened); a file opened without following a
//! symlink or waiting on a FIFO, owned by us with no other hard link, 0600.

use std::fs::File;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::logging::{effective_uid, open_no_follow, refuse_unless_ours};

/// Where state goes when `HOME` is unset: `/tmp` is shared, so the root is
/// named for the user and checked like the directory itself.
pub fn no_home_root() -> PathBuf {
    no_home_root_for(effective_uid())
}

fn no_home_root_for(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/snitchwatch-{uid}"))
}

/// Create `dir` (and any parents) 0700, then make sure `dir` is a real
/// directory of ours, 0700. Under [`no_home_root`] that root is held to the
/// same rule, since `/tmp` lets anyone create it first.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let root = no_home_root();
    for ancestor in dir.ancestors().filter(|a| a.starts_with(&root)) {
        tighten_own_dir(ancestor)?;
    }
    tighten_own_dir(dir)
}

fn tighten_own_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(refused(dir, "isn't a directory"));
    }
    if meta.uid() != effective_uid() {
        return Err(refused(dir, "belongs to another user"));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn refused(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} {why}, so it was left as it is", path.display()),
    )
}

/// Open `path` for appending, creating it 0600, in a private directory.
/// Refuses a symlink, a FIFO, a file of another user and a hard-linked file.
pub fn open_private_append(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    let file = open_no_follow(
        path,
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600),
    )?;
    refuse_unless_ours(path, &file)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::Builder::new()
            .prefix("priv")
            .tempdir_in(base.canonicalize().unwrap())
            .unwrap()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn an_existing_open_directory_is_tightened_to_0700() {
        let dir = scratch();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&state).unwrap();
        assert_eq!(mode(&state), 0o700);
    }

    #[test]
    fn a_new_directory_and_its_new_parents_are_0700() {
        let dir = scratch();
        let state = dir.path().join("a/b/state");
        ensure_private_dir(&state).unwrap();
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&dir.path().join("a")), 0o700);
    }

    #[test]
    fn a_symlink_in_place_of_the_directory_is_refused() {
        let dir = scratch();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let state = dir.path().join("state");
        std::os::unix::fs::symlink(&target, &state).unwrap();
        assert!(ensure_private_dir(&state).is_err());
        assert_eq!(mode(&target), 0o755, "the link's target was not touched");
    }

    #[test]
    fn a_file_in_place_of_the_directory_is_refused() {
        let dir = scratch();
        let state = dir.path().join("state");
        std::fs::write(&state, "x").unwrap();
        assert!(ensure_private_dir(&state).is_err());
    }

    #[test]
    fn the_no_home_root_is_per_user() {
        assert_eq!(
            no_home_root_for(1000),
            PathBuf::from("/tmp/snitchwatch-1000")
        );
        assert_ne!(no_home_root_for(1000), no_home_root_for(1001));
    }

    #[test]
    fn an_appended_file_is_0600_and_keeps_its_contents() {
        use std::io::Write;
        let dir = scratch();
        let path = dir.path().join("state/crash.log");
        open_private_append(&path)
            .unwrap()
            .write_all(b"one\n")
            .unwrap();
        open_private_append(&path)
            .unwrap()
            .write_all(b"two\n")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn a_symlink_a_hard_link_or_a_fifo_is_not_written_through() {
        let dir = scratch();
        let state = dir.path().join("state");
        ensure_private_dir(&state).unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep me").unwrap();

        let link = state.join("crash.log");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(open_private_append(&link).is_err());
        std::fs::remove_file(&link).unwrap();

        std::fs::hard_link(&victim, &link).unwrap();
        assert!(open_private_append(&link).is_err());
        std::fs::remove_file(&link).unwrap();

        // SAFETY: a valid NUL-terminated path.
        let fifo = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(open_private_append(&link).is_err());

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
    }
}
