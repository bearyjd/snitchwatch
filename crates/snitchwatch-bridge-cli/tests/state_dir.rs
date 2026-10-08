//! The real binary writes `blocklists.sqlite3` (issue #45) and
//! `profiles.sqlite3` (issue #46) only in its configured state directory,
//! owner-only, and nowhere without one.
//!
//! Every child runs with a cleared environment plus only what it needs: `PATH`,
//! a temp `XDG_RUNTIME_DIR` and `current_dir`, a relative WS socket, an
//! ephemeral gRPC bind and (per test) `STATE_DIRECTORY` /
//! `SNITCHWATCH_STATE_DIR`. A bridge started without that isolation would
//! replace a running bridge's socket and token (CLAUDE.md).

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_snitchwatch-bridge-cli");
const DEADLINE: Duration = Duration::from_secs(10);
const DBS: [&str; 2] = ["blocklists.sqlite3", "profiles.sqlite3"];

/// Kills and reaps the child on drop, so a failed assertion never leaks a
/// running bridge.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the bridge in `dir` with `extra` env vars and wait until it has
/// printed its last discovery line (the store is open by then).
fn start_bridge(dir: &Path, extra: &[(&str, &Path)]) -> ChildGuard {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", dir)
        .env("SNITCHWATCH_WS_SOCKET", "./bridge.sock")
        .env("SNITCHWATCH_GRPC_BIND", "127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let mut child = ChildGuard(cmd.spawn().expect("spawn snitchwatch-bridge-cli"));
    let stdout = child.0.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let start = Instant::now();
    loop {
        let remaining = DEADLINE
            .checked_sub(start.elapsed())
            .expect("no WS_TOKEN_PATH= line within the deadline");
        match rx.recv_timeout(remaining) {
            Ok(line) if line.starts_with("WS_TOKEN_PATH=") => return child,
            Ok(_) => {}
            Err(e) => panic!("bridge exited or stalled before its discovery lines: {e}"),
        }
    }
}

/// Every `blocklists.sqlite3` and `profiles.sqlite3` under `root`, sorted.
fn databases(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|n| DBS.iter().any(|db| n == *db))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn assert_only_database_in(root: &Path, state: &Path) {
    let state = state.canonicalize().unwrap();
    let expected: Vec<PathBuf> = DBS.iter().map(|db| state.join(db)).collect();
    let found: Vec<PathBuf> = databases(root)
        .into_iter()
        .map(|p| p.canonicalize().unwrap())
        .collect();
    assert_eq!(found, expected);
    for db in &expected {
        let mode = std::fs::metadata(db).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} must be owner-only", db.display());
    }
}

#[test]
fn state_directory_holds_the_only_database() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let _bridge = start_bridge(root.path(), &[("STATE_DIRECTORY", &state)]);
    assert_only_database_in(root.path(), &state);
}

#[test]
fn snitchwatch_state_dir_is_used_without_state_directory() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("manual");
    std::fs::create_dir(&state).unwrap();
    let _bridge = start_bridge(root.path(), &[("SNITCHWATCH_STATE_DIR", &state)]);
    assert_only_database_in(root.path(), &state);
}

#[test]
fn no_state_directory_writes_no_database() {
    let root = tempfile::tempdir().unwrap();
    let _bridge = start_bridge(root.path(), &[]);
    assert!(databases(root.path()).is_empty());
}

/// A misconfigured state directory never stops the bridge.
#[test]
fn a_missing_state_directory_still_starts_the_bridge() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    let _bridge = start_bridge(root.path(), &[("STATE_DIRECTORY", &missing)]);
    assert!(databases(root.path()).is_empty());
    assert!(!missing.exists());
}
