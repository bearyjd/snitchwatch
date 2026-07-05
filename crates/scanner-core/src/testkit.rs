//! Deterministic test doubles for [`crate::inspector::SystemInspector`].
//!
//! Kept in the shipped library (not `#[cfg(test)]`) so that both this
//! crate's unit tests *and* its integration tests (`tests/`) — plus
//! `scanner-cli`'s tests — can drive the scanner against canned host state
//! with no real rpm-ostree/ostree/flatpak/ss present. It has no production
//! call sites.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::inspector::{CommandOutput, InspectError, SystemInspector};

/// A programmable [`SystemInspector`]. Register command outputs, file
/// contents, and directory listings; anything unregistered behaves like a
/// missing binary / missing file.
#[derive(Default)]
pub struct MockInspector {
    /// Keyed by `"cmd arg1 arg2"` (space-joined).
    commands: HashMap<String, CommandOutput>,
    files: HashMap<PathBuf, String>,
    dirs: HashMap<PathBuf, Vec<PathBuf>>,
    available: Mutex<Vec<String>>,
}

impl MockInspector {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(cmd: &str, args: &[&str]) -> String {
        if args.is_empty() {
            cmd.to_string()
        } else {
            format!("{cmd} {}", args.join(" "))
        }
    }

    /// Register a command's output and mark the binary available.
    pub fn with_command(mut self, cmd: &str, args: &[&str], output: CommandOutput) -> Self {
        self.mark_available(cmd);
        self.commands.insert(Self::key(cmd, args), output);
        self
    }

    /// Convenience: register a successful command returning `stdout`.
    pub fn with_stdout(self, cmd: &str, args: &[&str], stdout: &str) -> Self {
        self.with_command(
            cmd,
            args,
            CommandOutput {
                stdout: stdout.to_string(),
                stderr: String::new(),
                success: true,
                code: Some(0),
            },
        )
    }

    pub fn with_file(mut self, path: impl Into<PathBuf>, contents: &str) -> Self {
        self.files.insert(path.into(), contents.to_string());
        self
    }

    pub fn with_dir(mut self, path: impl Into<PathBuf>, entries: Vec<PathBuf>) -> Self {
        self.dirs.insert(path.into(), entries);
        self
    }

    /// Mark a binary as present on `PATH` without registering any output.
    pub fn mark_available(&self, cmd: &str) {
        let mut guard = self.available.lock().expect("mock lock");
        if !guard.iter().any(|c| c == cmd) {
            guard.push(cmd.to_string());
        }
    }
}

impl SystemInspector for MockInspector {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<CommandOutput, InspectError> {
        if !self.which(cmd) {
            return Err(InspectError::NotAvailable {
                cmd: cmd.to_string(),
            });
        }
        Ok(self
            .commands
            .get(&Self::key(cmd, args))
            .cloned()
            .unwrap_or(CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                success: true,
                code: Some(0),
            }))
    }

    fn read_to_string(&self, path: &Path) -> Result<String, InspectError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| InspectError::Io {
                path: path.display().to_string(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            })
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, InspectError> {
        Ok(self.dirs.get(path).cloned().unwrap_or_default())
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
            || self.dirs.contains_key(path)
            || self.dirs.values().any(|v| v.iter().any(|p| p == path))
    }

    fn which(&self, cmd: &str) -> bool {
        self.available
            .lock()
            .expect("mock lock")
            .iter()
            .any(|c| c == cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_command_is_not_available() {
        let mock = MockInspector::new();
        assert!(!mock.which("rpm-ostree"));
        assert!(matches!(
            mock.run("rpm-ostree", &["status"]),
            Err(InspectError::NotAvailable { .. })
        ));
    }

    #[test]
    fn registered_command_returns_stdout() {
        let mock = MockInspector::new().with_stdout("echo", &["hi"], "hi\n");
        let out = mock.run("echo", &["hi"]).unwrap();
        assert_eq!(out.stdout, "hi\n");
        assert!(out.success);
    }
}
