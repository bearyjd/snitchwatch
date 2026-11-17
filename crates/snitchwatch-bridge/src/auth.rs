//! Shared-secret handshake token for the WS `/stream` endpoint.
//!
//! The bridge generates a fresh random token at startup and writes it to a
//! file under `$XDG_RUNTIME_DIR/snitchwatch/` (mode 0600, parent dir 0700)
//! rather than exposing it via an environment variable. That choice is
//! deliberate: once the WS transport moved from a TCP loopback socket to a
//! Unix domain socket (see the Phase 1 design note), the GUI client may be
//! running inside a Flatpak sandbox with its own process environment, so it
//! cannot read an env var set in the bridge's process. It *can* read a file
//! under the same runtime directory the sandbox is granted access to via
//! `--filesystem=xdg-run/snitchwatch`.
//!
//! NOTE (defense-in-depth, not the only guard): connecting over a Unix
//! domain socket means the kernel can hand back a verified peer UID via
//! `SO_PEERCRED` essentially for free. This token is kept as an additional
//! layer on top of filesystem/socket permissions (simpler to reason about,
//! and avoids the extra `SO_PEERCRED` syscall plumbing) — it is not meant to
//! be the sole thing standing between an attacker and the WS `/stream`
//! endpoint.

use rand::RngCore;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Number of random bytes in a generated token, before hex-encoding.
const TOKEN_BYTES: usize = 32;

/// A generated handshake token.
///
/// Cheap to clone (backed by a `String`) so it can be shared across the
/// `WsServer` and each connection's handshake check.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Generate a new random token using the OS RNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        Token(hex_encode(&bytes))
    }

    /// Build a token from an already-known string (tests, or re-reading a
    /// previously written token file).
    pub fn from_str_unchecked(s: impl Into<String>) -> Self {
        Token(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison against a presented string, so a malformed
    /// or wrong token doesn't leak timing information about how much of it
    /// matched.
    pub fn matches(&self, presented: &str) -> bool {
        let a = self.0.as_bytes();
        let b = presented.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the actual secret in logs/debug output.
        f.debug_tuple("Token").field(&"<redacted>").finish()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The directory the bridge's Unix socket and token file live under.
///
/// Defaults to `$XDG_RUNTIME_DIR/snitchwatch`. Falls back to a directory
/// under the OS temp dir when `XDG_RUNTIME_DIR` isn't set (uncommon on a
/// real desktop session, but happens in some CI/test environments; bridge-cli
/// dev-TCP mode relies on it). That shared directory is safe to use only
/// because `write_token_file` refuses a symlink, foreign-owned, or non-0700
/// `snitchwatch` directory.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("snitchwatch"),
        None => std::env::temp_dir().join("snitchwatch"),
    }
}

/// Write `token` to `path`, creating the parent directory (mode 0700) if
/// needed. Whatever sits at `path` is removed first and a fresh file is
/// created exclusively (`O_EXCL | O_NOFOLLOW`) at mode 0600, so a planted
/// symlink is never written through.
///
/// The parent must be a real directory (never a symlink) owned by the
/// effective uid; it ends up mode 0700. Otherwise nothing is written.
pub fn write_token_file(token: &Token, path: &Path) -> io::Result<()> {
    write_token_file_as(token, path, unsafe { libc::geteuid() })
}

fn write_token_file_as(token: &Token, path: &Path, euid: u32) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_private_dir(parent, euid)?;
    }
    // Never write through whatever sits at `path`: a symlink (or a file the
    // loose directory let someone else plant) is removed, then a fresh file is
    // created exclusively, without following links, already at mode 0600.
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if file.metadata()?.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("token file {} is not owned by uid {euid}", path.display()),
        ));
    }
    file.write_all(token.as_str().as_bytes())?;
    Ok(())
}

/// Create `dir` (0700) if missing, then require it to be a real directory
/// owned by `euid`, tightening an own-owned directory to exactly 0700.
/// Ancestors are created normally; only the final component is guarded.
///
/// The final component is opened with `O_NOFOLLOW | O_DIRECTORY` and checked
/// and chmodded through that descriptor, so a symlink planted there is never
/// followed and nothing behind it is touched. A directory owned by anyone
/// else is refused rather than repaired. (Unlike `check_socket_dir` in
/// `snitchwatch-bridge-cli`'s `user_socket.rs`, which only checks, an existing
/// own-owned directory with a looser mode is tightened: callers have always
/// pointed the token at pre-existing directories such as a fresh tempdir.
/// That crate depends on this one, so the two checks stay separate copies.)
fn ensure_private_dir(dir: &Path, euid: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if let Some(ancestors) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(ancestors)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let deny = |msg: String| io::Error::new(io::ErrorKind::PermissionDenied, msg);
    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => deny(format!(
                "token directory {} is not a real directory (symlink or file)",
                dir.display()
            )),
            _ => e,
        })?;
    let meta = handle.metadata()?;
    if meta.uid() != euid {
        return Err(deny(format!(
            "token directory {} is owned by uid {}, not {euid}",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o7777 != 0o700 {
        handle.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomically replace the system bridge token without touching its service-owned
/// parent directory. The caller's setgid runtime directory supplies the UI
/// group; `O_NOFOLLOW`/`create_new` prevent following an attacker-made entry.
pub fn write_system_token_file(token: &Token, path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "token has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "token has no filename"))?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    let result = (|| {
        if file.metadata()?.gid() != fs::metadata(parent)?.gid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "system token group did not inherit auth directory group",
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o640))?;
        file.write_all(token.as_str().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        drop(file);
        // create_new above proves this temporary entry belongs to this write.
        // Preserve any existing published token when initialization fails.
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Read a previously written token file back (trims trailing whitespace/
/// newlines so a stray editor/echo newline doesn't break the comparison).
pub fn read_token_file(path: &Path) -> io::Result<Token> {
    let raw = fs::read_to_string(path)?;
    Ok(Token::from_str_unchecked(raw.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_not_trivially_equal() {
        let a = Token::generate();
        let b = Token::generate();
        assert_ne!(a.as_str(), b.as_str());
        assert_eq!(a.as_str().len(), TOKEN_BYTES * 2, "hex-encoded length");
    }

    #[test]
    fn matches_accepts_correct_token() {
        let t = Token::generate();
        assert!(t.matches(t.as_str()));
    }

    #[test]
    fn matches_rejects_wrong_token() {
        let t = Token::generate();
        assert!(!t.matches("not-the-token"));
    }

    #[test]
    fn matches_rejects_different_length() {
        let t = Token::generate();
        let mut too_short = t.as_str().to_string();
        too_short.pop();
        assert!(!t.matches(&too_short));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snitchwatch").join("token");
        let token = Token::generate();
        write_token_file(&token, &path).unwrap();

        let read_back = read_token_file(&path).unwrap();
        assert!(token.matches(read_back.as_str()));

        let parent_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
    }

    fn euid() -> u32 {
        unsafe { libc::geteuid() }
    }

    #[test]
    fn a_symlinked_token_dir_is_refused_and_nothing_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = tmp.path().join("snitchwatch");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = write_token_file(&Token::generate(), &link.join("token")).unwrap_err();
        assert!(err.to_string().contains("not a real directory"), "{err}");
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755, "target must not be chmodded");
    }

    #[test]
    fn a_symlink_planted_at_the_token_path_is_replaced_not_written_through() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("snitchwatch");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        let victim = tmp.path().join("victim");
        fs::write(&victim, "precious").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.join("token");
        std::os::unix::fs::symlink(&victim, &path).unwrap();

        write_token_file(&Token::generate(), &path).unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
        let victim_mode = fs::metadata(&victim).unwrap().permissions().mode() & 0o7777;
        assert_eq!(victim_mode, 0o644, "symlink target must not be chmodded");
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_file(), "token must be a regular file");
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);
    }

    #[test]
    fn a_token_path_that_is_a_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("token");
        fs::create_dir(&path).unwrap();
        assert!(write_token_file(&Token::generate(), &path).is_err());
    }

    #[test]
    fn an_own_dir_with_a_loose_mode_is_tightened_to_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("snitchwatch");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        write_token_file(&Token::generate(), &dir.join("token")).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_dangling_symlink_token_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("snitchwatch");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).unwrap();
        assert!(write_token_file(&Token::generate(), &link.join("token")).is_err());
        assert!(!tmp.path().join("nowhere").exists());
    }

    #[test]
    fn a_token_dir_owned_by_someone_else_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("snitchwatch");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        // Claiming a different euid is the same mismatch as a foreign owner.
        let err =
            write_token_file_as(&Token::generate(), &dir.join("token"), euid() + 1).unwrap_err();
        assert!(err.to_string().contains("owned by uid"), "{err}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn missing_token_dirs_are_created_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a").join("b").join("token");
        write_token_file(&Token::generate(), &path).unwrap();
        let mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700);
    }

    #[test]
    fn debug_does_not_leak_token() {
        let t = Token::generate();
        let debug_str = format!("{t:?}");
        assert!(!debug_str.contains(t.as_str()));
    }
}
