//! Bounded, locale-stable execution of fixed system primitives. No shell.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
struct ActiveGroup {
    pid: i32,
    expired: bool,
}
static ACTIVE_GROUP: Mutex<ActiveGroup> = Mutex::new(ActiveGroup {
    pid: 0,
    expired: false,
});

/// The invocation watchdog also terminates the active primitive's descendants.
pub fn kill_active_group() {
    let mut state = ACTIVE_GROUP
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Serialize against spawn+registration, and refuse any later spawn.
    state.expired = true;
    if state.pid > 0 {
        unsafe {
            libc::killpg(state.pid, libc::SIGKILL);
        }
    }
}

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        {
            let mut state = ACTIVE_GROUP
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            // WNOWAIT kept this leader PID reserved, so it cannot name an
            // unrelated recycled group. Clear it before the final reap.
            unsafe {
                libc::killpg(self.0.id() as i32, libc::SIGKILL);
            }
            state.pid = 0;
        }
        let _ = self.0.wait();
    }
}

pub fn trusted_program(program: &str) -> Option<std::path::PathBuf> {
    // pkexec's environment is already sanitized; never trust caller PATH here.
    ["/usr/bin", "/usr/sbin"]
        .iter()
        .map(|dir| std::path::Path::new(dir).join(program))
        .find(|path| path.is_file())
}

pub fn run(program: &str, args: &[&str]) -> io::Result<String> {
    let path = if program.starts_with("/usr/bin/") || program.starts_with("/usr/sbin/") {
        std::path::PathBuf::from(program)
    } else {
        trusted_program(program).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "trusted system primitive not installed",
            )
        })?
    };
    let not_owned = if program == "rpm" && args.len() == 2 && args[0] == "-qf" {
        Some(format!("file {} is not owned by any package\n", args[1]))
    } else {
        None
    };
    capture(
        Command::new(path).args(args),
        Duration::from_secs(300),
        not_owned.as_deref(),
    )
}

fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, limit: usize) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if bytes.len() + n > limit {
                    return Err(io::Error::other(
                        "system primitive output exceeded byte limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

fn capture(
    command: &mut Command,
    timeout: Duration,
    not_owned: Option<&str>,
) -> io::Result<String> {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Isolate check descendants for cleanup; the root watchdog knows this group.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut state = ACTIVE_GROUP
        .lock()
        .map_err(|_| io::Error::other("primitive state poisoned"))?;
    if state.expired {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "scanner watchdog already expired",
        ));
    }
    if state.pid != 0 {
        return Err(io::Error::other("another system primitive is active"));
    }
    let mut child = Guard(command.spawn()?);
    state.pid = child.0.id() as i32;
    drop(state);
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let deadline = Instant::now() + timeout;
    loop {
        let out_done = drain(&mut stdout, &mut out, STDOUT_LIMIT)?;
        let err_done = drain(&mut stderr, &mut err, STDERR_LIMIT)?;
        // Observe without reaping: keep the group leader's PID reserved until
        // Guard kills descendants and clears the watchdog's numeric group ID.
        let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                child.0.id(),
                &mut status,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe { status.si_pid() } != 0 && out_done && err_done {
            let code = unsafe { status.si_status() };
            let unowned = status.si_code == libc::CLD_EXITED
                && code == 1
                && not_owned.is_some_and(|expected| out == expected.as_bytes())
                && err.is_empty();
            if !(status.si_code == libc::CLD_EXITED && code == 0) && !unowned {
                return Err(io::Error::other(format!(
                    "system primitive failed: code {} status {code}",
                    status.si_code
                )));
            }
            return String::from_utf8(out).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "system primitive returned invalid UTF-8",
                )
            });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "system primitive deadline exceeded",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn fixture(script: &str) -> io::Result<String> {
        capture(
            Command::new("/usr/bin/sh").args(["-c", script]),
            Duration::from_millis(250),
            None,
        )
    }
    #[test]
    fn expired_watchdog_rejects_late_children() {
        let _lock = SERIAL.lock().unwrap();
        kill_active_group();
        assert_eq!(
            fixture("exit 0").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        let mut state = ACTIVE_GROUP.lock().unwrap();
        assert_eq!(state.pid, 0);
        state.expired = false;
    }
    #[test]
    fn expected_rpm_ownership_miss_and_normal_reaping() {
        let _lock = SERIAL.lock().unwrap();
        let expected = "file /fixture is not owned by any package\n";
        let output = capture(
            Command::new("/usr/bin/sh").args([
                "-c",
                "printf 'file /fixture is not owned by any package\\n'; exit 1",
            ]),
            Duration::from_millis(250),
            Some(expected),
        )
        .unwrap();
        assert_eq!(output, expected);
        assert!(capture(
            Command::new("/usr/bin/sh").args([
                "-c",
                "printf 'file /wrong is not owned by any package\\n'; exit 1"
            ]),
            Duration::from_millis(250),
            Some(expected)
        )
        .is_err());
        let pid: i32 = fixture("printf '%s' $$").unwrap().parse().unwrap();
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert_eq!(ACTIVE_GROUP.lock().unwrap().pid, 0);
    }
    #[test]
    fn rejects_status_and_misleading_output() {
        let _lock = SERIAL.lock().unwrap();
        assert!(fixture("exit 1").is_err());
        assert!(fixture("printf 'SecureBoot disabled'; exit 1").is_err());
        assert!(fixture("printf 'not owned by any package'; exit 1").is_err());
        assert!(fixture("printf 'INFECTED'; exit 1").is_err());
        assert_eq!(fixture("printf 'INFECTED'").unwrap(), "INFECTED");
    }
    #[test]
    fn rejects_invalid_utf8_and_missing_primitive() {
        let _lock = SERIAL.lock().unwrap();
        assert!(fixture("printf '\\377'").is_err());
        assert_eq!(
            run("snitchwatch-no-such-command", &[]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
    #[test]
    fn bounds_both_streams_and_child_held_pipe() {
        let _lock = SERIAL.lock().unwrap();
        assert!(fixture("head -c 1048577 /dev/zero").is_err());
        assert!(fixture("head -c 65537 /dev/zero >&2").is_err());
        let start = Instant::now();
        assert_eq!(
            fixture("sleep 30 & exit 0").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(ACTIVE_GROUP.lock().unwrap().pid, 0);
    }
}
