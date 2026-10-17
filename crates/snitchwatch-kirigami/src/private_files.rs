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
    resolve_private_dir(dir).map(|_| ())
}

/// [`ensure_private_dir`], returning the directory to use. A symlink in
/// place of `dir` (stow, dotfile managers) is followed exactly one hop, to a
/// real directory of ours. That target is never chmodded: if it is open to
/// group or others it is refused rather than silently narrowed (only a
/// directory that isn't a symlink is tightened to 0700). Under
/// [`no_home_root`] a symlink is never followed.
fn resolve_private_dir(dir: &Path) -> io::Result<PathBuf> {
    let root = no_home_root();
    if dir.starts_with(&root) {
        ensure_private_root(&root)?;
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    for ancestor in dir.ancestors().filter(|a| a.starts_with(&root)) {
        tighten_own_dir(ancestor)?;
    }
    if !dir.starts_with(&root) && std::fs::symlink_metadata(dir)?.is_symlink() {
        let target = follow_one_hop(dir)?;
        check_followed_dir(&target)?;
        return Ok(target);
    }
    tighten_own_dir(dir)?;
    Ok(dir.to_path_buf())
}

/// Where the symlink `link` points, read once (a relative target is taken
/// from the link's parent). The target must be a real directory, not another
/// symlink: a chain is refused, not walked.
fn follow_one_hop(link: &Path) -> io::Result<PathBuf> {
    let raw = std::fs::read_link(link)?;
    let target = match link.parent() {
        Some(parent) => parent.join(&raw),
        None => raw,
    };
    let meta = std::fs::symlink_metadata(&target)?;
    if meta.file_type().is_symlink() {
        return Err(refused(
            &target,
            "is itself a symlink (only one hop is followed)",
        ));
    }
    if !meta.is_dir() {
        return Err(refused(&target, "isn't a directory"));
    }
    Ok(target)
}

/// A followed target must be ours and closed to group and others already.
fn check_followed_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.uid() != effective_uid() {
        return Err(refused(dir, "belongs to another user"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(refused(
            dir,
            "is reached through a symlink and is open to group or others (chmod 700 it yourself)",
        ));
    }
    Ok(())
}

/// Make `root` (one level, not recursive, so a planted symlink is never
/// followed into creating something) and require it to be a real directory of
/// ours, 0700.
pub fn ensure_private_root(root: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    tighten_own_dir(root)
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

/// How many temp names are tried before giving up on a name clash.
const TEMP_ATTEMPTS: u32 = 8;

fn temp_name(file_name: &str, attempt: u32) -> String {
    format!(".{file_name}.tmp-{}-{attempt}", std::process::id())
}

/// Removes the temp file on drop (an error or a panic) unless disarmed.
struct TempGuard<'a>(Option<&'a Path>);

impl Drop for TempGuard<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Write `body` to `path` atomically, in a private directory: a private temp
/// file (`O_EXCL`, no-follow, 0600) beside it is written and synced, then
/// renamed over `path`, so a crash leaves the old content whole. `path` itself
/// must not be a symlink, FIFO, file of another user or hard-linked file. These
/// final-path checks are policy, not an accident of the open: the rename
/// replaces the directory entry, so without them a planted link or hard link
/// would simply be replaced. A symlinked *directory* is followed one hop only
/// (see [`ensure_private_dir`]) and must already be private.
pub fn write_private(path: &Path, body: &[u8]) -> io::Result<()> {
    write_private_with(path, body, || Ok(()), sync_dir)
}

/// Flush the directory entry created by the rename.
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// [`write_private`] with a hook run after the temp file is synced and before
/// the rename, so tests can fail or crash at exactly that point; `after_rename`
/// syncs the directory (a failure is a write error, but the renamed file stays).
fn write_private_with(
    path: &Path,
    body: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
    after_rename: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .ok_or_else(|| refused(path, "has no file name"))?;
    let name_str = name.to_string_lossy();
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => resolve_private_dir(parent)?,
        _ => resolve_private_dir(Path::new("."))?,
    };
    let target = dir.join(name);
    check_final_path(&target, &name_str)?;

    let mut attempt = 0;
    let (mut file, temp) = loop {
        let temp = dir.join(temp_name(&name_str, attempt));
        match open_no_follow(
            &temp,
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600),
        ) {
            Ok(file) => break (file, temp),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt >= TEMP_ATTEMPTS {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    };
    let mut guard = TempGuard(Some(&temp));
    file.write_all(body)?;
    file.sync_all()?;
    before_rename()?;
    std::fs::rename(&temp, &target)?;
    guard.0 = None;
    after_rename(&dir)
}

/// The final path, if it exists, must be a regular file of ours with one name.
fn check_final_path(target: &Path, name: &str) -> io::Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{name} is a symlink; Snitchwatch writes only regular files it owns ({})",
                target.display()
            ),
        )),
        Ok(_) => {
            let existing = open_no_follow(target, std::fs::OpenOptions::new().read(true))?;
            refuse_unless_ours(target, &existing)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// A directory owned by someone other than us, for tests, under any uid: `/usr`
/// when we aren't root, else a temp directory chowned to `nobody` (root can).
/// The returned guard keeps a temp directory alive.
#[cfg(test)]
pub(crate) fn foreign_owned_dir() -> (PathBuf, Option<tempfile::TempDir>) {
    let (path, guard) = if effective_uid() == 0 {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("foreign")
            .tempdir_in(base.canonicalize().unwrap())
            .unwrap();
        let c = std::ffi::CString::new(dir.path().to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::chown(c.as_ptr(), 65534, u32::MAX) }, 0);
        (dir.path().to_path_buf(), Some(dir))
    } else {
        (PathBuf::from("/usr"), None)
    };
    assert_ne!(
        std::fs::metadata(&path).unwrap().uid(),
        effective_uid(),
        "the fixture must belong to someone else"
    );
    (path, guard)
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

    /// #176: a symlinked directory we own resolves to a real directory that
    /// is then held to the same rule (ours, tightened to 0700).
    #[test]
    fn a_symlinked_dir_is_resolved_and_its_target_tightened() {
        let dir = scratch();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = dir.path().join("state");
        std::os::unix::fs::symlink(&target, &state).unwrap();
        ensure_private_dir(&state).unwrap();
        assert_eq!(mode(&target), 0o700);
        assert!(std::fs::symlink_metadata(&state).unwrap().is_symlink());
        // A relative link target is read from the link's parent.
        let rel = dir.path().join("rel");
        std::os::unix::fs::symlink("elsewhere", &rel).unwrap();
        assert_eq!(resolve_private_dir(&rel).unwrap(), target);
    }

    /// A followed target is never chmodded: open to others, it is refused.
    #[test]
    fn a_followed_target_open_to_others_is_refused_not_narrowed() {
        let dir = scratch();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).unwrap();
        let state = dir.path().join("state");
        std::os::unix::fs::symlink(&target, &state).unwrap();
        let error = ensure_private_dir(&state).unwrap_err();
        assert!(
            error.to_string().contains("open to group or others"),
            "{error}"
        );
        assert_eq!(mode(&target), 0o750, "the target was not narrowed");
        assert!(write_private(&state.join("settings.json"), b"x").is_err());
        assert!(!target.join("settings.json").exists());
    }

    #[test]
    fn a_two_hop_symlink_chain_is_refused() {
        let dir = scratch();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hop1 = dir.path().join("hop1");
        std::os::unix::fs::symlink(&real, &hop1).unwrap();
        let hop2 = dir.path().join("hop2");
        std::os::unix::fs::symlink(&hop1, &hop2).unwrap();
        let error = ensure_private_dir(&hop2).unwrap_err();
        assert!(error.to_string().contains("only one hop"), "{error}");
        assert!(ensure_private_dir(&hop1).is_ok());
    }

    #[test]
    fn a_failed_directory_sync_is_an_error_but_keeps_the_renamed_file() {
        let dir = scratch();
        let path = dir.path().join("settings.json");
        write_private(&path, b"old").unwrap();
        let result =
            write_private_with(&path, b"new", || Ok(()), |_| Err(io::Error::other("sync")));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(names(dir.path()), vec!["settings.json"]);
        assert!(sync_dir(dir.path()).is_ok());
        assert!(sync_dir(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn a_symlink_to_a_foreign_dir_or_a_file_is_refused() {
        let dir = scratch();
        let (foreign, _guard) = foreign_owned_dir();
        let before = mode(&foreign);
        let state = dir.path().join("state");
        std::os::unix::fs::symlink(&foreign, &state).unwrap();
        assert!(ensure_private_dir(&state).is_err());
        assert_eq!(mode(&foreign), before, "a foreign target is left alone");

        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let to_file = dir.path().join("to_file");
        std::os::unix::fs::symlink(&file, &to_file).unwrap();
        assert!(ensure_private_dir(&to_file).is_err());
    }

    #[test]
    fn a_dangling_symlink_in_place_of_the_directory_is_refused() {
        let dir = scratch();
        let state = dir.path().join("state");
        std::os::unix::fs::symlink(dir.path().join("missing"), &state).unwrap();
        assert!(ensure_private_dir(&state).is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[test]
    fn writing_through_a_symlinked_parent_lands_in_the_real_dir() {
        let dir = scratch();
        let real = dir.path().join("dotfiles");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = dir.path().join("cfg");
        std::os::unix::fs::symlink(&real, &cfg).unwrap();
        write_private(&cfg.join("settings.json"), b"hello").unwrap();
        assert_eq!(std::fs::read(real.join("settings.json")).unwrap(), b"hello");
        assert_eq!(mode(&real.join("settings.json")), 0o600);
        assert_eq!(mode(&real), 0o700);
    }

    #[test]
    fn a_symlinked_file_gets_a_clear_message() {
        let dir = scratch();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = write_private(&link, b"x").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is a symlink; Snitchwatch writes only regular files it owns"),
            "{error}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// #176: the file is replaced by a rename, so a failure or crash before
    /// it leaves the old content whole, and no temp file is left behind.
    #[test]
    fn a_failure_before_the_rename_keeps_the_old_content_and_leaves_no_temp() {
        let dir = scratch();
        let path = dir.path().join("settings.json");
        write_private(&path, b"old").unwrap();
        let result = write_private_with(
            &path,
            b"new",
            || {
                // The temp is fully written by now; the old file is untouched.
                assert_eq!(std::fs::read(&path).unwrap(), b"old");
                Err(io::Error::other("injected"))
            },
            sync_dir,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(names(dir.path()), vec!["settings.json"]);
    }

    #[test]
    fn a_crash_mid_write_keeps_the_old_content() {
        let dir = scratch();
        let path = dir.path().join("settings.json");
        write_private(&path, b"old").unwrap();
        let crashed = std::panic::catch_unwind(|| {
            let _ = write_private_with(&path, b"new", || panic!("crash"), sync_dir);
        });
        assert!(crashed.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(names(dir.path()), vec!["settings.json"]);
    }

    /// `O_EXCL`: a regular file already sitting at every temp name is never
    /// reused or truncated.
    #[test]
    fn an_existing_temp_file_is_never_reused() {
        let dir = scratch();
        let path = dir.path().join("settings.json");
        for attempt in 0..TEMP_ATTEMPTS {
            std::fs::write(dir.path().join(temp_name("settings.json", attempt)), "mine").unwrap();
        }
        assert!(write_private(&path, b"x").is_err());
        assert!(!path.exists());
        for attempt in 0..TEMP_ATTEMPTS {
            let temp = dir.path().join(temp_name("settings.json", attempt));
            assert_eq!(std::fs::read(temp).unwrap(), b"mine");
        }
    }

    #[test]
    fn the_temp_file_is_private_and_a_planted_one_is_not_followed() {
        let dir = scratch();
        let path = dir.path().join("settings.json");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        // Plant symlinks at every temp name the first attempts would use.
        for attempt in 0..TEMP_ATTEMPTS {
            let planted = dir.path().join(temp_name("settings.json", attempt));
            std::os::unix::fs::symlink(&victim, &planted).unwrap();
        }
        assert!(write_private(&path, b"x").is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(!path.exists());
        write_private_with(
            &dir.path().join("other.json"),
            b"y",
            || {
                let temps: Vec<_> = names(dir.path())
                    .into_iter()
                    .filter(|n| n.contains("other.json.tmp"))
                    .collect();
                assert_eq!(temps.len(), 1);
                assert_eq!(mode(&dir.path().join(&temps[0])), 0o600);
                Ok(())
            },
            sync_dir,
        )
        .unwrap();
    }

    #[test]
    fn a_file_in_place_of_the_directory_is_refused() {
        let dir = scratch();
        let state = dir.path().join("state");
        std::fs::write(&state, "x").unwrap();
        assert!(ensure_private_dir(&state).is_err());
    }

    #[test]
    fn a_planted_symlink_root_is_refused_and_nothing_is_made_under_it() {
        let dir = scratch();
        let target = dir.path().join("attacker");
        std::fs::create_dir(&target).unwrap();
        let root = dir.path().join("snitchwatch-1000");
        std::os::unix::fs::symlink(&target, &root).unwrap();
        assert!(ensure_private_root(&root).is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[test]
    fn a_written_file_is_0600_and_replaces_its_contents() {
        let dir = scratch();
        let path = dir.path().join("cfg/settings.json");
        write_private(&path, b"first contents").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn writing_through_a_symlink_or_into_a_foreign_dir_is_refused() {
        let dir = scratch();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_private(&link, b"x").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let (foreign, _guard) = foreign_owned_dir();
        assert!(write_private(&foreign.join("settings.json"), b"x").is_err());
        assert!(!foreign.join("settings.json").exists());
    }

    #[test]
    fn writing_to_a_hard_linked_file_is_refused() {
        let dir = scratch();
        let original = dir.path().join("original");
        std::fs::write(&original, "keep").unwrap();
        let path = dir.path().join("settings.json");
        std::fs::hard_link(&original, &path).unwrap();
        assert!(write_private(&path, b"x").is_err());
        assert_eq!(std::fs::read(&original).unwrap(), b"keep");
    }

    #[test]
    fn a_root_made_by_someone_else_is_refused() {
        let (foreign, _guard) = foreign_owned_dir();
        assert!(ensure_private_root(&foreign).is_err());
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
