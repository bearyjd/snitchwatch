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
use std::io;
use std::os::unix::fs::PermissionsExt;
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
/// real desktop session, but happens in some CI/test environments).
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("snitchwatch"),
        None => std::env::temp_dir().join("snitchwatch"),
    }
}

/// Write `token` to `path`, creating the parent directory (mode 0700) if
/// needed and tightening the file itself to mode 0600.
pub fn write_token_file(token: &Token, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(path, token.as_str())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
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

    #[test]
    fn debug_does_not_leak_token() {
        let t = Token::generate();
        let debug_str = format!("{t:?}");
        assert!(!debug_str.contains(t.as_str()));
    }
}
