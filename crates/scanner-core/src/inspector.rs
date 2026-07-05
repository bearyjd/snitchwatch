//! Abstraction over the host: running external tools and reading files.
//!
//! Every shell-out the scanner performs (`rpm-ostree status --json`,
//! `rpm -V`, `flatpak override --show`, `ss`, …) goes through
//! [`SystemInspector`]. Production code uses [`RealSystem`]; tests use
//! [`crate::testkit::MockInspector`]. This is what keeps the classification
//! and diffing logic testable on a machine with no rpm-ostree.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// Captured result of an external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    /// Exit status success (`ExitStatus::success()`).
    pub success: bool,
    pub code: Option<i32>,
}

#[derive(Debug, Error)]
pub enum InspectError {
    #[error("command `{cmd}` is not available on this host")]
    NotAvailable { cmd: String },
    #[error("failed to spawn `{cmd}`: {source}")]
    Spawn {
        cmd: String,
        #[source]
        source: std::io::Error,
    },
    #[error("io error for `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Host inspection surface. Kept intentionally small and side-effect-only —
/// no parsing lives here so it stays trivial to mock.
pub trait SystemInspector: Send + Sync {
    /// Run `cmd args...`, capturing stdout/stderr. A non-zero exit is *not*
    /// an error (many of our tools, e.g. `rpm -V`, exit non-zero to signal
    /// findings, not failure); inspect [`CommandOutput::success`] instead.
    /// Only a missing binary or spawn failure is an `Err`.
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput, InspectError>;

    /// Read a file to a UTF-8 string (lossy for non-UTF-8 bytes).
    fn read_to_string(&self, path: &Path) -> Result<String, InspectError>;

    /// List a directory's immediate children (files and subdirs), sorted.
    /// A missing directory yields an empty list, not an error.
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, InspectError>;

    /// Whether a path exists.
    fn exists(&self, path: &Path) -> bool;

    /// Whether an executable named `cmd` is resolvable on `PATH`.
    fn which(&self, cmd: &str) -> bool;
}

/// Production [`SystemInspector`] backed by `std::process` and `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealSystem;

impl SystemInspector for RealSystem {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput, InspectError> {
        if !self.which(cmd) {
            return Err(InspectError::NotAvailable {
                cmd: cmd.to_string(),
            });
        }
        let output =
            Command::new(cmd)
                .args(args)
                .output()
                .map_err(|source| InspectError::Spawn {
                    cmd: cmd.to_string(),
                    source,
                })?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            success: output.status.success(),
            code: output.status.code(),
        })
    }

    fn read_to_string(&self, path: &Path) -> Result<String, InspectError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(source) => Err(InspectError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, InspectError> {
        let read = match std::fs::read_dir(path) {
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(InspectError::Io {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        let mut entries: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn which(&self, cmd: &str) -> bool {
        // Absolute/relative path given directly.
        if cmd.contains('/') {
            return Path::new(cmd).is_file();
        }
        let Some(paths) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_real_binary() {
        let sys = RealSystem;
        // `sh` is present essentially everywhere we'd run.
        assert!(sys.which("sh"));
        assert!(!sys.which("definitely-not-a-real-binary-xyzzy"));
    }

    #[test]
    fn list_dir_missing_is_empty_not_error() {
        let sys = RealSystem;
        let out = sys
            .list_dir(Path::new("/nonexistent/xyzzy/path"))
            .expect("missing dir is Ok(empty)");
        assert!(out.is_empty());
    }

    #[test]
    fn run_missing_binary_is_not_available() {
        let sys = RealSystem;
        let err = sys
            .run("definitely-not-a-real-binary-xyzzy", &[])
            .unwrap_err();
        assert!(matches!(err, InspectError::NotAvailable { .. }));
    }
}
