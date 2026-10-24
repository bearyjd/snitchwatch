//! Resource boundary for untrusted MMDB parsing, including metadata decoding.
//!
//! The normal installed/system database discovery path remains trusted. Imports
//! are checked in an early mode of this same executable before any GUI, logging,
//! bridge, or background-thread initialization. No input becomes a path/command.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(super) const FLAG: &str = "--snitchwatch-validate-country-database";
const MAX_INPUT: usize = 32 * 1024 * 1024;
const MAX_OUTPUT: usize = 8192;
const WALL_LIMIT: Duration = Duration::from_secs(60);
const LIMIT_ERROR: &str = "Database validation exceeded its resource limit. The existing database was preserved; choose another country database.";

pub(super) fn validate(bytes: &[u8]) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|e| format!("Could not locate the database validator: {e}"))?;
    let mut command = Command::new(executable);
    command.arg(FLAG);
    validate_with_command(bytes, command, WALL_LIMIT)
}

// Kept separate so integration tests exercise the real producer/consumer with
// CARGO_BIN_EXE and short deadlines, without changing production limits/argv.
pub(super) fn validate_with_command(
    bytes: &[u8],
    mut command: Command,
    deadline: Duration,
) -> Result<(), String> {
    if bytes.len() > MAX_INPUT {
        return Err("The selected database exceeds 32 MiB.".into());
    }
    let started = Instant::now();
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start the database validator: {e}"))?;
    let mut child = ReapedChild(child);
    let mut input = child.0.stdin.take();
    let mut output = child.0.stdout.take().unwrap();
    let mut errors = child.0.stderr.take().unwrap();
    nonblocking(input.as_ref().unwrap())?;
    nonblocking(&output)?;
    nonblocking(&errors)?;
    let mut sent = 0;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        if started.elapsed() >= deadline {
            return Err(LIMIT_ERROR.into());
        }
        if let Some(pipe) = input.as_mut() {
            match pipe.write(&bytes[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => input = None,
                Err(e) => return Err(format!("Could not send the database for validation: {e}")),
            }
            if sent == bytes.len() {
                input = None; // EOF is the worker's frame boundary.
            }
        }
        drain(&mut output, &mut stdout)?;
        drain(&mut errors, &mut stderr)?;
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            // After exit, consume the final bytes before interpreting a reply.
            drain(&mut output, &mut stdout)?;
            drain(&mut errors, &mut stderr)?;
            if !status.success() {
                return Err(LIMIT_ERROR.into());
            }
            if sent != bytes.len() || !stderr.is_empty() {
                return Err("The database validator did not complete successfully.".into());
            }
            return serde_json::from_slice::<Result<(), String>>(&stdout)
                .map_err(|_| "The database validator returned an invalid response.".to_string())?;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<(), String> {
    // SAFETY: each fd belongs to a live, owned child pipe. Preserve its flags.
    let result = unsafe {
        let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            -1
        } else {
            libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
        }
    };
    if result < 0 {
        Err(format!(
            "Could not configure the database validator: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> Result<(), String> {
    // At most MAX_OUTPUT+1 bytes per stream: a noisy child cannot keep this
    // loop alive or allocate indefinitely, even before the wall check.
    let mut chunk = [0; 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if bytes.len() + n > MAX_OUTPUT {
                    return Err("The database validator exceeded its response limit.".into());
                }
                bytes.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => {
                return Err(format!(
                    "Could not read the database validator response: {e}"
                ))
            }
        }
    }
}

struct ReapedChild(Child);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        // Every error path, timeout and unwinding path terminates and reaps.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn run(check: fn(&[u8]) -> Result<(), String>) -> i32 {
    let result = set_limits().and_then(|()| {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Could not read the selected database: {e}"))?;
        if bytes.len() > MAX_INPUT {
            return Err("The selected database exceeds 32 MiB.".into());
        }
        check(&bytes)
    });
    // Library errors may quote untrusted strings. Bound the protocol result.
    let result = result.map_err(|e| e.chars().take(512).collect::<String>());
    let mut stdout = std::io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &result).is_err() || stdout.flush().is_err() {
        1
    } else {
        0
    }
}

fn set_limits() -> Result<(), String> {
    // Linux/Flatpak both support these per-process limits. Fail closed if a
    // launcher has imposed a lower hard limit rather than parsing unbounded.
    for (resource, value) in [
        (libc::RLIMIT_DATA, 256 * 1024 * 1024),
        (libc::RLIMIT_CPU, 30),
        (libc::RLIMIT_CORE, 0),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: valid resource and initialized rlimit pointer; single thread.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err("Could not apply database validation resource limits.".into());
        }
    }
    Ok(())
}
