//! The per-user gRPC Unix socket (#35): directory checks and bind.
//!
//! The socket lives in `$XDG_RUNTIME_DIR/snitchwatch-grpc/`, a sibling of the
//! WS directory: the Flatpak is granted all of `xdg-run/snitchwatch`, and a
//! sandboxed GUI running as the same uid would pass any uid check and could
//! pose as the daemon. Binding replaces a running bridge's gRPC socket, the
//! same as the WS socket and token already do (issue #34).

use anyhow::{bail, Context, Result};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;
use tokio::net::UnixListener;

const DIR_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;

/// `dir` must be a real directory (not a symlink), owned by `euid`, with
/// mode exactly 0700. Uses `lstat`, never following a final symlink.
pub(crate) fn check_socket_dir(dir: &Path, euid: u32) -> Result<()> {
    let meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("cannot stat gRPC socket directory {}", dir.display()))?;
    if !meta.file_type().is_dir() {
        bail!(
            "gRPC socket directory {} is not a real directory (symlink or file)",
            dir.display()
        );
    }
    if meta.uid() != euid {
        bail!(
            "gRPC socket directory {} is owned by uid {}, not {euid}",
            dir.display(),
            meta.uid()
        );
    }
    let mode = meta.mode() & 0o7777;
    if mode != DIR_MODE {
        bail!(
            "gRPC socket directory {} has mode {mode:04o}, must be 0700",
            dir.display()
        );
    }
    Ok(())
}

/// Create the directory (0700) if missing, check it, remove a stale socket,
/// bind, and tighten the socket to 0600. Must run inside a tokio runtime.
pub(crate) fn bind_user_socket(path: &Path) -> Result<UnixListener> {
    bind_user_socket_as(path, unsafe { libc::geteuid() })
}

fn bind_user_socket_as(path: &Path, euid: u32) -> Result<UnixListener> {
    let dir = path
        .parent()
        .context("gRPC socket path has no parent directory")?;
    match std::fs::DirBuilder::new().mode(DIR_MODE).create(dir) {
        // A strict umask can clear bits `mode()` asked for, so set 0700
        // explicitly on a directory we just made. An existing one is only
        // checked, never repaired.
        Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
            .with_context(|| format!("cannot chmod gRPC socket directory {}", dir.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot create gRPC socket directory {}", dir.display()))
        }
    }
    check_socket_dir(dir, euid)?;
    remove_stale_socket(path)?;
    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("cannot bind gRPC socket {}", path.display()))?;
    // The 0700 directory covers the window between bind and chmod.
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE)) {
        // Never leave a socket with an unintended mode behind.
        let _ = std::fs::remove_file(path);
        return Err(e).with_context(|| format!("cannot chmod gRPC socket {}", path.display()));
    }
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener).context("cannot adopt the gRPC Unix listener")
}

/// Only a socket is removed; anything else at the path is an error.
fn remove_stale_socket(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("cannot remove stale socket {}", path.display())),
        Ok(_) => bail!("{} exists and is not a socket", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot stat {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn euid() -> u32 {
        unsafe { libc::geteuid() }
    }

    fn dir_with_mode(parent: &Path, name: &str, mode: u32) -> std::path::PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        dir
    }

    #[test]
    fn a_0700_directory_owned_by_us_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_with_mode(tmp.path(), "ok", 0o700);
        check_socket_dir(&dir, euid()).unwrap();
    }

    #[test]
    fn a_symlinked_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = dir_with_mode(tmp.path(), "real", 0o700);
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        assert!(check_socket_dir(&link, euid()).is_err());
        // Binding through it must also refuse, and leave nothing in `real`.
        assert!(bind_user_socket(&link.join("opensnitchd.sock")).is_err());
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_with_the_wrong_mode_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for mode in [0o755, 0o750, 0o770, 0o500] {
            let dir = dir_with_mode(tmp.path(), &format!("m{mode:o}"), mode);
            assert!(check_socket_dir(&dir, euid()).is_err(), "mode {mode:o}");
        }
    }

    #[test]
    fn a_directory_with_the_wrong_owner_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_with_mode(tmp.path(), "theirs", 0o700);
        // The directory is ours; claiming a different euid is the same
        // mismatch as a directory owned by someone else.
        assert!(check_socket_dir(&dir, euid() + 1).is_err());
        assert!(bind_user_socket_as(&dir.join("opensnitchd.sock"), euid() + 1).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_owned_by_someone_else_is_refused() {
        use std::os::unix::fs::chown;
        if euid() != 0 {
            eprintln!("SKIPPED: needs root to chown a directory to another uid");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = dir_with_mode(tmp.path(), "theirs", 0o700);
        chown(&dir, Some(65534), Some(65534)).unwrap();
        let err = check_socket_dir(&dir, euid()).unwrap_err().to_string();
        assert!(err.contains("owned by uid 65534"), "{err}");
        assert!(bind_user_socket(&dir.join("opensnitchd.sock")).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    // Runs in a fresh process so the umask change cannot touch other tests.
    #[test]
    fn strict_umask_helper() {
        let Some(root) = std::env::var_os("SNITCHWATCH_TEST_UMASK_DIR") else {
            return;
        };
        unsafe { libc::umask(0o277) };
        let path = Path::new(&root)
            .join("snitchwatch-grpc")
            .join("opensnitchd.sock");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let _listener = bind_user_socket(&path).unwrap();
        let mode = std::fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_strict_umask_still_yields_a_0700_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "user_socket::tests::strict_umask_helper",
                "--nocapture",
            ])
            .env("SNITCHWATCH_TEST_UMASK_DIR", tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn bind_creates_a_0700_directory_and_a_0600_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snitchwatch-grpc").join("opensnitchd.sock");
        let _listener = bind_user_socket(&path).unwrap();
        let dir_meta = std::fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(dir_meta.mode() & 0o7777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    }

    #[tokio::test]
    async fn bind_replaces_a_stale_socket_but_never_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snitchwatch-grpc").join("opensnitchd.sock");
        drop(bind_user_socket(&path).unwrap());
        // The first listener's socket file is left behind: stale.
        assert!(path.exists());
        drop(bind_user_socket(&path).unwrap());

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"precious").unwrap();
        assert!(bind_user_socket(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"precious");
    }
}
