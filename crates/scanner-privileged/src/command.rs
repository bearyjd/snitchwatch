//! Bounded, locale-stable execution of fixed system primitives. No shell.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, TryLockError};
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

/// How long the watchdog sweeps the scanner's descendants before it kills
/// its own group regardless (a process stuck in the kernel can't die sooner).
const DESCENDANT_SWEEP: Duration = Duration::from_secs(2);

/// The root watchdog's kill: the active primitive's group, then every
/// descendant (`containment`: none is skipped for being unregistered, or for
/// the registry's lock being held), then the scanner's own group `own_group`
/// (its process group leader and pid, from `main`).
pub fn expire_watchdog(own_group: i32) {
    expire_watchdog_with(
        own_group,
        kill_active_group,
        |root| crate::containment::kill_descendants(root, DESCENDANT_SWEEP),
        |group| unsafe {
            libc::killpg(group, libc::SIGKILL);
        },
        |message| tracing::warn!("{message}"),
    );
}

/// [`expire_watchdog`] with the sweep, the group kill and the log injected.
/// Refuses a group of 0 (the caller's own) or 1 (init's): `killpg` on them, or
/// a sweep rooted at them, would take down far more than the scanner. Nothing
/// that can block (logging) runs before the final `killpg`: a stuck stderr
/// must not hold off the kill, so the sweep's diagnostics come after it.
/// The sweep (bounded per `/proc` entry and per pid) deliberately precedes the
/// own-group kill: that kill takes this process down with it, and anything in
/// another group or session (setsid, an orphan) would survive it.
fn expire_watchdog_with(
    own_group: i32,
    kill_active: impl FnOnce(),
    sweep: impl FnOnce(i32) -> crate::containment::Sweep,
    killpg: impl FnOnce(i32),
    log: impl Fn(&str),
) {
    // Before anything releases `capture`: see `wait_for_watchdog_cleanup`.
    let _scope = ExpiryScope::enter();
    kill_active();
    if own_group <= 1 {
        log(&format!(
            "watchdog refuses to kill invalid scanner group {own_group}"
        ));
        return;
    }
    let sweep = sweep(own_group);
    killpg(own_group);
    if !sweep.complete {
        log("watchdog descendant sweep did not finish before the own-group kill");
    }
    if let Some(first) = &sweep.first_error {
        log(&format!(
            "watchdog could not signal {} descendant(s); first error: {first}",
            sweep.failures
        ));
    }
}

/// Whether the watchdog is mid-expiry, so the main thread must not end the
/// process (and the watchdog thread with it) before the cleanup is done.
static WATCHDOG_EXPIRING: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// How long the main thread waits for the watchdog's cleanup: the sweep bound
/// plus a margin. In production the final `killpg` ends the process first.
const WATCHDOG_CLEANUP_WAIT: Duration = Duration::from_secs(5);

/// Marks the watchdog's cleanup as under way until dropped (even on panic).
struct ExpiryScope;
impl ExpiryScope {
    fn enter() -> Self {
        *WATCHDOG_EXPIRING
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = true;
        ExpiryScope
    }
}
impl Drop for ExpiryScope {
    fn drop(&mut self) {
        *WATCHDOG_EXPIRING
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = false;
        WATCHDOG_EXPIRING.1.notify_all();
    }
}

/// Call before the main thread returns or exits: if the watchdog is mid-expiry
/// (its first step releases `capture` with an error, which can bring `main` to
/// its end), wait, bounded, until its cleanup is done, so the process is not
/// ended mid-sweep with descendants alive.
pub fn wait_for_watchdog_cleanup() {
    let guard = WATCHDOG_EXPIRING
        .0
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _ = WATCHDOG_EXPIRING
        .1
        .wait_timeout_while(guard, WATCHDOG_CLEANUP_WAIT, |expiring| *expiring);
}

/// Test only: a pre-exec hook that reports the forked child's pid on this fd
/// and then stalls, as a hung exec would (`-1`: off). The value is copied at
/// fork, so only a primitive spawned after it is set stalls.
#[cfg(test)]
static STALL_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
/// Test only: hold off registering a spawned primitive's pid, as a
/// descheduled capture thread would.
#[cfg(test)]
static DELAY_REGISTRATION: AtomicBool = AtomicBool::new(false);
/// Test only: the watchdog "fires" right after the next spawn returns, and the
/// spawned pid is recorded here.
#[cfg(test)]
static EXPIRE_AFTER_SPAWN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SPAWNED_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

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

/// PDEATHSIG is cleared by exec of a setuid/setgid or file-capability binary,
/// so refuse to spawn one (fail closed). None of the primitives in use
/// (uname, modinfo, rpm, rpm-ostree, mokutil) is either on Fedora/Bazzite.
fn refuse_privileged_binary(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let denied = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing to run {}: {why}", path.display()),
        )
    };
    if path.metadata()?.mode() & (libc::S_ISUID | libc::S_ISGID) != 0 {
        return Err(denied("setuid/setgid binary"));
    }
    if has_xattr(path, "security.capability")? {
        return Err(denied("file capabilities set"));
    }
    Ok(())
}

fn has_xattr(path: &std::path::Path, name: &str) -> io::Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let to_c = |bytes: &[u8]| {
        std::ffi::CString::new(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
    };
    let (path, name) = (to_c(path.as_os_str().as_bytes())?, to_c(name.as_bytes())?);
    if unsafe { libc::getxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) } >= 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENODATA) | Some(libc::ENOTSUP) => Ok(false),
        _ => Err(e),
    }
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
    refuse_privileged_binary(std::path::Path::new(command.get_program()))?;
    command
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Isolate check descendants for cleanup; the root watchdog knows this group
    // once the spawn returns. Until then the parent has no pid for the child,
    // so it also dies with the scanner: PDEATHSIG fires when the forking
    // thread ends, and primitives run only on the scanner's main thread, which
    // lives as long as the process (capture is synchronous and Guard reaps
    // before it returns). PDEATHSIG is defence in depth, not the containment:
    // exec of a setuid/setgid or file-capability binary clears it (such a
    // binary is refused above), and forked descendants never inherit it. Either way the watchdog's descendant sweep
    // (`containment`, the scanner being a subreaper) still kills them, so
    // losing it fails safe.
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
    #[cfg(test)]
    if DELAY_REGISTRATION.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(60));
    }
    let mut state = ACTIVE_GROUP
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    state.spawning = false;
    let child = spawned?;
    // Unreaped, so this pid (and group) stays reserved until Guard clears it.
    state.pid = child.id() as i32;
    #[cfg(test)]
    if EXPIRE_AFTER_SPAWN.swap(false, Ordering::SeqCst) {
        SPAWNED_PID.store(state.pid, Ordering::SeqCst);
        EXPIRED.store(true, Ordering::SeqCst);
    }
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
    /// Gone, or a zombie waiting for whoever inherited it.
    fn gone(pid: i32) -> bool {
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
    }

    /// Whether `scanner` has exited, observed without reaping it, so its pid
    /// (and process group) stays reserved and a later `kill(-pid)` can never
    /// name a recycled, unrelated group. Polls until `bound` has passed.
    fn exited_within(scanner: &Child, bound: Duration) -> bool {
        let start = Instant::now();
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let done = unsafe {
                libc::waitid(
                    libc::P_PID,
                    scanner.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } == 0
                && unsafe { info.si_pid() } != 0;
            if done || start.elapsed() > bound {
                return done;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill the fixture's whole group while its pid is still reserved
    /// (unreaped), then reap it and return its status.
    fn kill_group_then_reap(scanner: &mut Child) -> std::process::ExitStatus {
        unsafe {
            libc::kill(-(scanner.id() as i32), libc::SIGKILL);
        }
        scanner.wait().unwrap()
    }

    /// A primitive whose spawn stalls after it left the scanner's group (a
    /// hung exec) must not hold off the root watchdog: the scanner dies within
    /// the bound, and so does the stalled child it never got a pid for. Runs in
    /// a child process, since the watchdog kills its own group. The watchdog
    /// fires only once the stalled child has reported its pid.
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
            // The stalled child reports into this pipe; the watchdog thread
            // forwards the pid to the test, then expires.
            let mut hook = [0; 2];
            unsafe {
                assert_eq!(libc::pipe2(hook.as_mut_ptr(), libc::O_CLOEXEC), 0);
            }
            STALL_FD.store(hook[1], std::sync::atomic::Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut pid: i32 = 0;
                loop {
                    let n = unsafe { libc::read(hook[0], (&mut pid as *mut i32).cast(), 4) };
                    if n == 4 {
                        break;
                    }
                    if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                unsafe {
                    libc::write(fd, (&pid as *const i32).cast(), 4);
                }
                expire_watchdog(own);
            });
            let _ = capture(
                Command::new("/usr/bin/sh").args(["-c", "exit 0"]),
                Duration::from_secs(60),
                None,
            );
            // Whether or not the sweep ended the spawn (killing the stalled
            // child fails it), only the watchdog's own-group kill ends this.
            loop {
                unsafe { libc::pause() };
            }
        }
        let mut pipe = [0; 2];
        unsafe {
            assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
            // The write end goes to the scanner fixture; the read end stays here.
            assert_eq!(libc::fcntl(pipe[1], libc::F_SETFD, 0), 0);
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
        let bound = Duration::from_secs(5);
        unsafe {
            assert_eq!(libc::fcntl(pipe[0], libc::F_SETFL, libc::O_NONBLOCK), 0);
        }
        let mut stalled: i32 = 0;
        let start = Instant::now();
        while stalled == 0 && start.elapsed() < Duration::from_secs(10) {
            let mut pid: i32 = 0;
            if unsafe { libc::read(pipe[0], (&mut pid as *mut i32).cast(), 4) } == 4 {
                stalled = pid;
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let exited = exited_within(&scanner, bound);
        let reaped = stalled > 0 && {
            let since = Instant::now();
            while !gone(stalled) && since.elapsed() < bound {
                std::thread::sleep(Duration::from_millis(10));
            }
            gone(stalled)
        };
        // Never leak a stuck fixture, whatever failed.
        if stalled > 0 && !gone(stalled) {
            unsafe {
                libc::kill(stalled, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        unsafe {
            libc::close(pipe[0]);
        }
        assert!(exited, "the stalled spawn held off the watchdog");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        assert!(stalled > 0, "the stalled child never reported its pid");
        assert!(reaped, "the stalled child {stalled} outlived the scanner");
    }

    /// The pids a fixture script wrote (`primitive helper`, newline-terminated
    /// so a half-written file is never taken for a complete one).
    fn reported_pids(file: &std::path::Path) -> Vec<i32> {
        match std::fs::read_to_string(file) {
            Ok(text) if text.ends_with('\n') => text
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A primitive that execs and forks a long-lived helper before the scanner
    /// has registered its pid: the watchdog fires, and within the bound the
    /// primitive, its helper (which PDEATHSIG doesn't reach) and the scanner
    /// are all dead. Twice: the primitive still waiting for its helper, and
    /// the primitive already gone, the helper orphaned (kept below the scanner
    /// only by the subreaper). Runs in a child scanner process; the watchdog
    /// fires only once the fixture has reached the state under test.
    #[test]
    fn a_forked_helper_cannot_outlive_the_watchdog() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "SNITCHWATCH_TEST_FORKED_HELPER";
        const MODE: &str = "SNITCHWATCH_TEST_FORKED_HELPER_MODE";
        if let Some(pids) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            DELAY_REGISTRATION.store(true, Ordering::SeqCst);
            let orphan = std::env::var(MODE).unwrap() == "orphan";
            let file = std::path::PathBuf::from(&pids);
            std::thread::spawn(move || {
                loop {
                    let reported = reported_pids(&file);
                    // The orphan state: the primitive has exited too.
                    if reported.len() == 2 && (!orphan || gone(reported[0])) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                expire_watchdog(own);
            });
            let then = if orphan { "exit 0" } else { "wait" };
            let script = format!(
                "sleep 60 & printf '%s %s\\n' $$ $! > {}; {then}",
                pids.to_str().unwrap()
            );
            let _ = capture(
                Command::new("/usr/bin/sh").args(["-c", &script]),
                Duration::from_secs(60),
                None,
            );
            loop {
                unsafe { libc::pause() };
            }
        }
        for mode in ["wait", "orphan"] {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let mut scanner = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "command::tests::a_forked_helper_cannot_outlive_the_watchdog",
                    "--test-threads=1",
                ])
                .env(CHILD, &pids)
                .env(MODE, mode)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let bound = Duration::from_secs(10);
            let start = Instant::now();
            let mut reported = Vec::new();
            while reported.len() != 2 && start.elapsed() < bound {
                reported = reported_pids(&pids);
                std::thread::sleep(Duration::from_millis(10));
            }
            let exited = exited_within(&scanner, Duration::from_secs(5));
            let since = Instant::now();
            while !reported.iter().all(|&pid| gone(pid)) && since.elapsed() < Duration::from_secs(5)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            let survivors: Vec<i32> = reported.iter().copied().filter(|&pid| !gone(pid)).collect();
            // Never leak a fixture, whatever failed.
            for &pid in &survivors {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            let status = kill_group_then_reap(&mut scanner);
            assert!(exited, "{mode}: the watchdog did not terminate the scanner");
            assert_eq!(status.signal(), Some(libc::SIGKILL), "{mode}: {status:?}");
            assert_eq!(
                reported.len(),
                2,
                "{mode}: the primitive never reported its pids"
            );
            assert!(
                survivors.is_empty(),
                "{mode}: outlived the watchdog (primitive, helper): {reported:?}, alive: {survivors:?}"
            );
        }
    }

    /// Processes that left the scanner's group (setsid), and one orphaned by
    /// its parent's death (kept below the scanner only by the subreaper), are
    /// all dead after the real watchdog fires. Runs in a child scanner.
    #[test]
    fn setsid_and_reparented_processes_die_with_the_watchdog() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "SNITCHWATCH_TEST_ESCAPEES";
        if let Some(file) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            let detached = |program: &str, args: &[&str]| {
                let mut command = Command::new(program);
                command
                    .args(args)
                    .stdin(Stdio::null())
                    .stderr(Stdio::null());
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                command
            };
            // A: out of the group and session, still our child.
            #[allow(clippy::zombie_processes)] // killed by the watchdog under test
            let a = detached("/usr/bin/sleep", &["60"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            // B: its parent exits, so it is reparented to this subreaper.
            let mut parent = detached("/usr/bin/sh", &["-c", "sleep 60 & echo $!"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut io::BufReader::new(parent.stdout.take().unwrap()),
                &mut line,
            )
            .unwrap();
            parent.wait().unwrap();
            let b: i32 = line.trim().parse().unwrap();
            while crate::containment::parent_and_zombie(b).map(|(p, _)| p) != Some(own) {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::write(&file, format!("{} {b}\n", a.id())).unwrap();
            expire_watchdog(own);
            loop {
                unsafe { libc::pause() };
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::setsid_and_reparented_processes_die_with_the_watchdog",
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let mut reported = Vec::new();
        while reported.len() != 2 && start.elapsed() < Duration::from_secs(10) {
            reported = reported_pids(&pids);
            std::thread::sleep(Duration::from_millis(10));
        }
        let exited = exited_within(&scanner, Duration::from_secs(5));
        let since = Instant::now();
        while !reported.iter().all(|&pid| gone(pid)) && since.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors: Vec<i32> = reported.iter().copied().filter(|&pid| !gone(pid)).collect();
        for &pid in &survivors {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        assert!(exited, "the watchdog did not terminate the scanner");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        assert_eq!(reported.len(), 2, "the fixture never reported its pids");
        assert!(survivors.is_empty(), "outlived the watchdog: {survivors:?}");
    }

    /// The main thread must not return (and so end the process, and the
    /// watchdog thread with it) while the watchdog is still cleaning up: the
    /// watchdog's first step releases `capture`, whose error would otherwise
    /// let `main` exit with a detached descendant still alive. The injected
    /// delay after the release makes the unsynchronised exit certain.
    #[test]
    fn main_waits_for_the_watchdog_to_finish_its_sweep() {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "SNITCHWATCH_TEST_MAIN_EXITS_EARLY";
        if let Some(file) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            let mut detached = Command::new("/usr/bin/sleep");
            detached
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null());
            unsafe {
                detached.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            #[allow(clippy::zombie_processes)] // killed by the watchdog under test
            let detached = detached.spawn().unwrap();
            std::fs::write(&file, format!("{}\n", detached.id())).unwrap();
            std::thread::spawn(move || {
                // Fire once the primitive below is registered.
                while ACTIVE_GROUP.lock().unwrap().pid == 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                expire_watchdog_with(
                    own,
                    || {
                        kill_active_group();
                        std::thread::sleep(Duration::from_millis(300));
                    },
                    |root| crate::containment::kill_descendants(root, DESCENDANT_SWEEP),
                    |group| unsafe {
                        libc::killpg(group, libc::SIGKILL);
                    },
                    |_| {},
                );
            });
            // Released by the watchdog's first step, with an error.
            let _ = capture(
                Command::new("/usr/bin/sh").args(["-c", "exec sleep 30"]),
                Duration::from_secs(60),
                None,
            );
            // What `main` does on its way out.
            wait_for_watchdog_cleanup();
            std::process::exit(0);
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::main_waits_for_the_watchdog_to_finish_its_sweep",
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let mut reported = Vec::new();
        while reported.is_empty() && start.elapsed() < Duration::from_secs(10) {
            reported = reported_pids(&pids);
            std::thread::sleep(Duration::from_millis(10));
        }
        let exited = exited_within(&scanner, Duration::from_secs(10));
        let since = Instant::now();
        while !reported.iter().all(|&pid| gone(pid)) && since.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors: Vec<i32> = reported.iter().copied().filter(|&pid| !gone(pid)).collect();
        for &pid in &survivors {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        assert!(exited, "the scanner never exited");
        assert_eq!(reported.len(), 1, "the fixture never reported its pid");
        assert!(
            survivors.is_empty(),
            "main exited before the watchdog's sweep: {survivors:?} survived ({status:?})"
        );
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
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

    /// A child that is always killed and reaped, even if an assertion panics.
    struct Reaped(Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn sweep_with(failures: u32) -> crate::containment::Sweep {
        crate::containment::Sweep {
            complete: failures == 0,
            failures,
            first_error: (failures > 0).then(|| io::Error::from_raw_os_error(libc::EPERM)),
        }
    }

    #[test]
    fn invalid_scanner_groups_never_reach_killpg_or_the_sweep() {
        let _lock = SERIAL.lock().unwrap();
        for group in [i32::MIN, -1, 0, 1] {
            let events = std::cell::RefCell::new(Vec::new());
            expire_watchdog_with(
                group,
                || events.borrow_mut().push("active".to_string()),
                |_| {
                    events.borrow_mut().push("sweep".to_string());
                    sweep_with(0)
                },
                |_| events.borrow_mut().push("killpg".to_string()),
                |m| events.borrow_mut().push(format!("log:{m}")),
            );
            let events = events.into_inner();
            assert_eq!(events.len(), 2, "group {group}: {events:?}");
            assert_eq!(events[0], "active");
            assert!(events[1].starts_with("log:"), "{events:?}");
        }
        EXPIRED.store(false, Ordering::SeqCst);
    }

    /// Nothing that can block (logging) may run before the final killpg; the
    /// sweep's failures are only reported afterwards.
    #[test]
    fn killpg_comes_before_every_diagnostic() {
        let _lock = SERIAL.lock().unwrap();
        for failures in [0, 3] {
            let events = std::cell::RefCell::new(Vec::new());
            expire_watchdog_with(
                4242,
                || events.borrow_mut().push("active".to_string()),
                |root| {
                    assert_eq!(root, 4242);
                    events.borrow_mut().push("sweep".to_string());
                    sweep_with(failures)
                },
                |g| events.borrow_mut().push(format!("killpg:{g}")),
                |m| events.borrow_mut().push(format!("log:{m}")),
            );
            let events = events.into_inner();
            assert_eq!(
                events[..3],
                ["active", "sweep", "killpg:4242"],
                "{events:?}"
            );
            // Unfinished sweep (failures > 0 here) is reported, but only after.
            assert_eq!(events.len() > 3, failures > 0, "{events:?}");
            assert!(events[3..].iter().all(|e| e.starts_with("log:")));
        }
        EXPIRED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn lock_timeout_lets_the_watchdog_move_on_without_killing() {
        let _lock = SERIAL.lock().unwrap();
        let mut victim = Reaped(Command::new("/usr/bin/sleep").arg("30").spawn().unwrap());
        let held = ACTIVE_GROUP.lock().unwrap();
        let start = Instant::now();
        kill_active_group();
        let waited = start.elapsed();
        drop(held);
        assert!(EXPIRED.load(Ordering::SeqCst));
        EXPIRED.store(false, Ordering::SeqCst);
        assert!(waited >= WATCHDOG_LOCK_WAIT, "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        assert!(victim.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn expiry_during_spawn_kills_the_registered_group() {
        let _lock = SERIAL.lock().unwrap();
        EXPIRE_AFTER_SPAWN.store(true, Ordering::SeqCst);
        let result = fixture("exec sleep 30");
        EXPIRED.store(false, Ordering::SeqCst);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let pid = SPAWNED_PID.load(Ordering::SeqCst);
        assert!(pid > 1);
        // Guard killed and reaped it: no such process (not even a zombie).
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
        assert_eq!(ACTIVE_GROUP.lock().unwrap().pid, 0);
    }

    #[test]
    fn setuid_setgid_and_capability_binaries_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prog");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let mode = |m| std::fs::set_permissions(&file, std::fs::Permissions::from_mode(m)).unwrap();
        mode(0o755);
        refuse_privileged_binary(&file).unwrap();
        for bits in [0o4755, 0o2755] {
            mode(bits);
            let _lock = SERIAL.lock().unwrap();
            let err =
                capture(&mut Command::new(&file), Duration::from_millis(250), None).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{bits:o}");
            assert_eq!(
                refuse_privileged_binary(&file).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        // The real primitives are not privileged.
        for name in ["sh", "sleep"] {
            refuse_privileged_binary(&trusted_program(name).unwrap()).unwrap();
        }
    }

    #[test]
    fn xattr_detection_reports_presence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(!has_xattr(&file, "user.snitchwatch-test").unwrap());
        let c = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        let set = unsafe {
            libc::setxattr(
                c.as_ptr(),
                c"user.snitchwatch-test".as_ptr(),
                b"1".as_ptr().cast(),
                1,
                0,
            )
        };
        if set != 0 {
            return; // this filesystem has no user xattrs
        }
        assert!(has_xattr(&file, "user.snitchwatch-test").unwrap());
    }
}
