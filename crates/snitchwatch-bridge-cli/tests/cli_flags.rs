//! Spawns the real `snitchwatch-bridge-cli` binary to prove two things:
//!
//! 1. `--help`/`-h`/`--version`/`-V` print and exit 0 *before any I/O* — in
//!    particular they leave no socket or token file behind (a bare `--help`
//!    used to start a full bridge and clobber a running bridge's socket + token).
//! 2. A stdout that is already closed (`--help | head -c0`) is not an error
//!    and not a panic, while any other write failure (a full disk) exits
//!    non-zero.
//! 3. The no-argument path still starts the bridge and prints its discovery
//!    lines, i.e. the early exit didn't change the default behavior.
//!
//! Every child runs fully isolated: `current_dir` and `XDG_RUNTIME_DIR` are a
//! fresh temp dir, the WS socket is a *relative* path (stays well under the
//! 108-byte `sun_path` limit), and the gRPC bind is ephemeral. Nothing here
//! may ever touch a live bridge's `$XDG_RUNTIME_DIR/snitchwatch/` or
//! `127.0.0.1:50051`.

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_snitchwatch-bridge-cli");
const DEADLINE: Duration = Duration::from_secs(5);

/// Kills and reaps the child on drop so a failed assertion (or a timeout
/// panic) can never leak a running bridge.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn isolated_command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .current_dir(dir)
        .env("SNITCHWATCH_WS_SOCKET", "./bridge.sock")
        .env("SNITCHWATCH_GRPC_BIND", "127.0.0.1:0")
        .env("XDG_RUNTIME_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn spawn_command(mut cmd: Command) -> ChildGuard {
    ChildGuard(cmd.spawn().expect("failed to spawn snitchwatch-bridge-cli"))
}

fn spawn(dir: &Path, args: &[&str]) -> ChildGuard {
    spawn_command(isolated_command(dir, args))
}

fn wait_with_deadline(child: &mut ChildGuard) -> ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().expect("try_wait failed") {
            return status;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "snitchwatch-bridge-cli did not exit within {DEADLINE:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Finished {
    stdout: String,
    stderr: String,
}

/// Run `flag` to completion in a fresh temp dir; assert a clean exit 0 and
/// that nothing was written into the dir.
fn run_flag(flag: &str) -> Finished {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = spawn(dir.path(), &[flag]);

    let status = wait_with_deadline(&mut child);
    assert!(status.success(), "`{flag}` exited with {status}");

    let mut stdout = String::new();
    child
        .0
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .expect("stderr was piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read temp dir")
        .map(|entry| entry.expect("dir entry").file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "`{flag}` must not do any I/O, but left {leftovers:?} behind"
    );

    Finished { stdout, stderr }
}

fn assert_prints_version(flag: &str) {
    let out = run_flag(flag);
    assert_eq!(
        out.stdout,
        format!("snitchwatch-bridge-cli {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(out.stderr, "", "`{flag}` must not log anything");
}

fn assert_prints_usage(flag: &str) {
    let out = run_flag(flag);
    for needle in [
        "SNITCHWATCH_GRPC_BIND",
        "SNITCHWATCH_WS_SOCKET",
        "GRPC_LISTEN_ADDR=",
    ] {
        assert!(
            out.stdout.contains(needle),
            "`{flag}` usage missing `{needle}`:\n{}",
            out.stdout
        );
    }
    assert_eq!(out.stderr, "", "`{flag}` must not log anything");
}

#[test]
fn long_help_prints_usage_without_io() {
    assert_prints_usage("--help");
}

#[test]
fn short_help_prints_usage_without_io() {
    assert_prints_usage("-h");
}

#[test]
fn long_version_prints_version_line_without_io() {
    assert_prints_version("--version");
}

#[test]
fn short_version_prints_version_line_without_io() {
    assert_prints_version("-V");
}

/// Run `flag` with its stdout replaced by `stdout`; returns the exit status
/// and everything the child wrote to stderr.
fn run_flag_with_stdout(flag: &str, stdout: Stdio) -> (ExitStatus, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = isolated_command(dir.path(), &[flag]);
    cmd.stdout(stdout);
    let mut child = spawn_command(cmd);

    let status = wait_with_deadline(&mut child);
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .expect("stderr was piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    (status, stderr)
}

/// stdout is a pipe whose read end is already closed, so the child's first
/// write fails with EPIPE every time (no race with a reader exiting). Rust
/// ignores SIGPIPE, so a `print!` here would panic: the flags must treat a
/// closed pipe as "nobody is listening", i.e. success.
fn assert_closed_stdout_is_success(flag: &str) {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let (status, stderr) = run_flag_with_stdout(flag, Stdio::from(writer));
    assert!(
        status.success(),
        "`{flag}` with a closed stdout exited with {status}"
    );
    assert_eq!(
        stderr, "",
        "`{flag}` with a closed stdout must exit quietly, not panic"
    );
}

#[test]
fn help_with_closed_stdout_exits_zero_without_panicking() {
    assert_closed_stdout_is_success("--help");
}

#[test]
fn version_with_closed_stdout_exits_zero_without_panicking() {
    assert_closed_stdout_is_success("--version");
}

/// A write failure that is *not* a closed pipe must not be swallowed.
/// `/dev/full` fails every write with ENOSPC.
#[cfg(target_os = "linux")]
#[test]
fn version_with_a_failing_stdout_exits_nonzero() {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("open /dev/full");
    let (status, stderr) = run_flag_with_stdout("--version", Stdio::from(full));
    assert_eq!(
        status.code(),
        Some(1),
        "exit status {status}, stderr: {stderr}"
    );
    assert!(
        stderr.contains("failed to write to stdout"),
        "stderr should say what failed, got: {stderr}"
    );
}

/// Control: with no flags the bridge must still start. Reads stdout until the
/// first discovery line, then the guard kills the child.
#[test]
fn no_args_still_starts_the_bridge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = isolated_command(dir.path(), &[]);
    // Nothing reads stderr here; an unread pipe could fill (64 KiB) and wedge
    // the child.
    cmd.stderr(Stdio::null());
    let mut child = spawn_command(cmd);

    let stdout = child.0.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    // Ends on its own once the guard kills the child and the pipe closes.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let start = Instant::now();
    let addr = loop {
        let remaining = DEADLINE
            .checked_sub(start.elapsed())
            .expect("no GRPC_LISTEN_ADDR= line within the deadline");
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(addr) = line.strip_prefix("GRPC_LISTEN_ADDR=") {
                    break addr.to_string();
                }
            }
            Err(e) => panic!("bridge produced no GRPC_LISTEN_ADDR= line: {e}"),
        }
    };

    let addr: SocketAddr = addr.parse().expect("GRPC_LISTEN_ADDR is a socket address");
    assert!(addr.ip().is_loopback(), "unexpected bind address {addr}");
    assert_ne!(addr.port(), 0, "ephemeral port was not resolved");
}
