//! Bounded, locale-stable execution of fixed system primitives. No shell.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
/// The registered primitive's group. Held only briefly, never across
/// `spawn()` (whose exec handshake can stall): the watchdog takes it to kill a
/// group that `Guard` cannot reap and clear meanwhile, so the numeric group id
/// can never name a recycled, unrelated group.
struct ActiveGroup {
    pid: i32,
    /// A spawn is under way; no second primitive may start.
    spawning: bool,
}
static ACTIVE_GROUP: Mutex<ActiveGroup> = Mutex::new(ActiveGroup {
    pid: 0,
    spawning: false,
});
/// Set first by the watchdog, without the lock: no primitive spawns after it,
/// and one whose spawn was under way is killed as soon as it registers.
static EXPIRED: AtomicBool = AtomicBool::new(false);
/// How long the watchdog waits for the lock before it moves on to the
/// scanner's own group (the lock is only ever held briefly).
const WATCHDOG_LOCK_WAIT: Duration = Duration::from_millis(100);

/// The root watchdog's kill: the active primitive's group, then the scanner's
/// own group `own_group` (its process group leader, from `main`).
pub fn expire_watchdog(own_group: i32) {
    kill_active_group();
    unsafe {
        libc::killpg(own_group, libc::SIGKILL);
    }
}

/// Test only: a pre-exec hook that reports the forked child's pid on this fd
/// and then stalls, as a hung exec would (`-1`: off). The value is copied at
/// fork, so only a primitive spawned after it is set stalls.
#[cfg(test)]
static STALL_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The invocation watchdog also terminates the active primitive's descendants.
/// Bounded: it never waits more than [`WATCHDOG_LOCK_WAIT`] for the lock.
pub fn kill_active_group() {
    EXPIRED.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + WATCHDOG_LOCK_WAIT;
    loop {
        let state = match ACTIVE_GROUP.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            // A primitive registering right now sees EXPIRED and kills itself.
            Err(TryLockError::WouldBlock) => return,
        };
        if state.pid > 0 {
            unsafe {
                libc::killpg(state.pid, libc::SIGKILL);
            }
        }
        return;
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

const TRUSTED_DIRS: [&str; 2] = ["/usr/bin", "/usr/sbin"];

/// A single path component: joining it to a trusted directory cannot leave it.
fn bare_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

pub fn trusted_program(program: &str) -> Option<std::path::PathBuf> {
    // pkexec's environment is already sanitized; never trust caller PATH here.
    if !bare_name(program) {
        return None;
    }
    TRUSTED_DIRS
        .iter()
        .map(|dir| std::path::Path::new(dir).join(program))
        .find(|path| path.is_file())
}

/// Accept a bare name, or an absolute path naming a direct child of a trusted
/// directory; anything else could escape it (`/usr/bin/../../tmp/x`).
fn resolve(program: &str) -> io::Result<std::path::PathBuf> {
    if !program.contains('/') {
        return trusted_program(program).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "trusted system primitive not installed",
            )
        });
    }
    let direct_child = TRUSTED_DIRS.iter().any(|dir| {
        program
            .strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(bare_name)
    });
    if !direct_child {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "system primitive path is outside the trusted directories",
        ));
    }
    Ok(std::path::PathBuf::from(program))
}

pub fn run(program: &str, args: &[&str]) -> io::Result<String> {
    let path = resolve(program)?;
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
    // Isolate check descendants for cleanup; the root watchdog knows this group
    // once the spawn returns. Until then the parent has no pid for the child,
    // so the child dies with the scanner: PDEATHSIG fires when the forking
    // thread ends, and `capture` runs synchronously on it and reaps (Guard)
    // before returning, so that is when the scanner dies. The trusted
    // primitives are not setuid/setgid or file-capability binaries, whose exec
    // would clear it.
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The scanner may have died before the line above took effect.
            if libc::getppid() != parent {
                libc::_exit(127);
            }
            #[cfg(test)]
            {
                let fd = STALL_FD.load(std::sync::atomic::Ordering::SeqCst);
                if fd >= 0 {
                    let pid = libc::getpid();
                    libc::write(fd, (&pid as *const i32).cast(), 4);
                    loop {
                        libc::pause();
                    }
                }
            }
            Ok(())
        });
    }
    let expired = || io::Error::new(io::ErrorKind::TimedOut, "scanner watchdog already expired");
    {
        let mut state = ACTIVE_GROUP
            .lock()
            .map_err(|_| io::Error::other("primitive state poisoned"))?;
        if EXPIRED.load(Ordering::SeqCst) {
            return Err(expired());
        }
        if state.pid != 0 || state.spawning {
            return Err(io::Error::other("another system primitive is active"));
        }
        state.spawning = true;
    }
    // Unlocked: a stalled exec handshake must not hold off the watchdog.
    let spawned = command.spawn();
    let mut state = ACTIVE_GROUP
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    state.spawning = false;
    let child = spawned?;
    // Unreaped, so this pid (and group) stays reserved until Guard clears it.
    state.pid = child.id() as i32;
    drop(state);
    let mut child = Guard(child);
    if EXPIRED.load(Ordering::SeqCst) {
        // The watchdog fired during the spawn; Guard kills the group.
        return Err(expired());
    }
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
        assert_eq!(ACTIVE_GROUP.lock().unwrap().pid, 0);
        EXPIRED.store(false, Ordering::SeqCst);
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
    /// A primitive whose spawn stalls after it left the scanner's group (a
    /// hung exec) must not hold off the root watchdog: the scanner dies within
    /// the bound, and so does the stalled child it never got a pid for. Runs in
    /// a child process, since the watchdog kills its own group.
    #[test]
    fn a_spawn_stall_cannot_hold_off_the_watchdog() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "SNITCHWATCH_TEST_SPAWN_STALL";
        if let Some(fd) = std::env::var_os(CHILD) {
            let fd: i32 = fd.to_str().unwrap().parse().unwrap();
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            STALL_FD.store(fd, std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                expire_watchdog(own);
            });
            let _ = capture(
                Command::new("/usr/bin/sh").args(["-c", "exit 0"]),
                Duration::from_secs(60),
                None,
            );
            // The hook never lets the spawn finish; only the watchdog ends this.
            unsafe { libc::_exit(3) };
        }
        let mut pipe = [0; 2];
        unsafe {
            assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
            // The write end goes to the scanner fixture; the read end stays here.
            assert_eq!(libc::fcntl(pipe[1], libc::F_SETFD, 0), 0);
            assert_eq!(libc::fcntl(pipe[0], libc::F_SETFL, libc::O_NONBLOCK), 0);
        }
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::a_spawn_stall_cannot_hold_off_the_watchdog",
                "--test-threads=1",
            ])
            .env(CHILD, pipe[1].to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        unsafe {
            libc::close(pipe[1]);
        }
        let scanner_pid = scanner.id() as i32;
        let start = Instant::now();
        let bound = Duration::from_secs(5);
        let mut stalled: i32 = 0;
        let status = loop {
            if stalled == 0 {
                let mut pid: i32 = 0;
                if unsafe { libc::read(pipe[0], (&mut pid as *mut i32).cast(), 4) } == 4 {
                    stalled = pid;
                }
            }
            if let Some(status) = scanner.try_wait().unwrap() {
                break Some(status);
            }
            if start.elapsed() > bound {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let gone = |pid: i32| {
            // Gone, or a zombie waiting for whoever inherited it.
            (unsafe { libc::kill(pid, 0) }) != 0
                || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .map(|stat| {
                        stat.rsplit(')')
                            .next()
                            .unwrap_or("")
                            .trim_start()
                            .starts_with('Z')
                    })
                    .unwrap_or(true)
        };
        let reaped = stalled > 0 && {
            let since = Instant::now();
            while !gone(stalled) && since.elapsed() < bound {
                std::thread::sleep(Duration::from_millis(10));
            }
            gone(stalled)
        };
        // Never leak a stuck fixture, whatever failed.
        unsafe {
            if stalled > 0 && !gone(stalled) {
                libc::kill(stalled, libc::SIGKILL);
            }
            libc::kill(-scanner_pid, libc::SIGKILL);
            libc::close(pipe[0]);
        }
        let _ = scanner.wait();
        let status = status.expect("the stalled spawn held off the watchdog");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        assert!(stalled > 0, "the stalled child never reported its pid");
        assert!(reaped, "the stalled child {stalled} outlived the scanner");
    }

    #[test]
    fn program_paths_cannot_escape_the_trusted_directories() {
        assert!(trusted_program("sh").is_some());
        // These resolve to real files today, so the check must be syntactic.
        for escape in ["/usr/bin/sh", "../bin/sh", "./sh", "", ".", ".."] {
            assert_eq!(trusted_program(escape), None, "{escape:?}");
        }
        for escape in [
            "/usr/bin/../../tmp/payload",
            "/usr/bin/./sh",
            "/usr/bin/nested/sh",
            "/usr/bin/",
            "/tmp/payload",
        ] {
            assert_eq!(
                run(escape, &[]).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{escape:?}"
            );
        }
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
