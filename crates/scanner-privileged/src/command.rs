//! Bounded, locale-stable execution of fixed system primitives. No shell.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
/// The scanner's watchdog_state: one state, one lock, mutually exclusive
/// transitions. Held only briefly, never across `spawn()` (whose exec
/// handshake can stall) or a sweep, so the numeric group id in `pid` can never
/// name a recycled, unrelated group (`Guard` clears it under the same lock).
///
/// ```text
/// Running  --watchdog fires-->  Expiring  --cleanup done-->  Expired
/// Running  --main leaves---->   Exiting
/// Expiring --main leaves---->   (waits, bounded, for Expired)
/// Exiting  --watchdog fires-->  (nothing: main handles the sweep)
/// ```
/// A spawn is authorized only in `Running` (it then marks itself `in_flight`
/// until its pid is registered or the spawn failed), so the watchdog, which
/// waits for `in_flight` to clear before it kills and sweeps, can never be
/// followed by a primitive it did not see.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Running,
    Expiring,
    Expired,
    Exiting,
}
struct WatchdogState {
    phase: Phase,
    /// A spawn is authorized and not yet registered; no second may start.
    in_flight: bool,
    /// The registered primitive's group (its leader's pid); 0: none.
    pid: i32,
}
static WATCHDOG_STATE: (Mutex<WatchdogState>, Condvar) = (
    Mutex::new(WatchdogState {
        phase: Phase::Running,
        in_flight: false,
        pid: 0,
    }),
    Condvar::new(),
);
/// Nothing blocks under this lock, so a poisoned one still holds a coherent
/// state. Silent: the watchdog must not log before its kill.
fn watchdog_state() -> MutexGuard<'static, WatchdogState> {
    WATCHDOG_STATE
        .0
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
/// Lock-free mirror of "the watchdog has begun to expire the scanner", set
/// under the watchdog_state lock on `Running` -> `Expiring` and never cleared. A
/// capture re-reads it right before it spawns, and a forked child before it
/// execs (an atomic load is async-signal-safe), so a capture that resumes
/// after the watchdog gave up waiting for it cannot start a primitive.
static EXPIRING: AtomicBool = AtomicBool::new(false);
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
///
/// `notify` is a best-effort, non-blocking diagnostic run only once the expiry
/// has actually begun (not when the scanner is already leaving), before any kill.
pub fn expire_watchdog(own_group: i32, notify: impl FnOnce()) {
    expire_watchdog_with(
        own_group,
        || {
            notify();
            kill_active_group();
        },
        |root| crate::containment::kill_descendants(root, DESCENDANT_SWEEP),
        |group| unsafe {
            libc::killpg(group, libc::SIGKILL);
        },
        |message| tracing::warn!("{message}"),
    );
}

/// Fires the group kill when dropped (even while unwinding from a panic in
/// the sweep), or earlier via [`GroupKill::fire`].
struct GroupKill<F: FnOnce(i32)> {
    killpg: Option<F>,
    group: i32,
}
impl<F: FnOnce(i32)> GroupKill<F> {
    fn fire(&mut self) {
        if let Some(killpg) = self.killpg.take() {
            killpg(self.group);
        }
    }
}
impl<F: FnOnce(i32)> Drop for GroupKill<F> {
    fn drop(&mut self) {
        self.fire();
    }
}

/// [`expire_watchdog`] with the sweep, the group kill and the log injected.
/// `Running` -> `Expiring` first (a late watchdog during an exit does
/// nothing), then it waits, bounded, for an in-flight spawn to register, so
/// nothing authorized earlier appears after the sweep. Refuses a group of 0
/// (the caller's own) or 1 (init's): `killpg` on them, or a sweep rooted at
/// them, would take down far more than the scanner. Nothing that can block
/// (logging) runs before the final `killpg`: a stuck stderr must not hold off
/// the kill, so the sweep's diagnostics come after it. The sweep (bounded per
/// `/proc` entry and per pid) deliberately precedes the own-group kill: that
/// kill takes this process down with it, and anything in another group or
/// session (setsid, an orphan) would survive it. The kill also fires if the
/// sweep panics.
fn expire_watchdog_with(
    own_group: i32,
    kill_active: impl FnOnce(),
    sweep: impl FnOnce(i32) -> crate::containment::Sweep,
    killpg: impl FnOnce(i32),
    log: impl Fn(&str),
) {
    let Some(_scope) = ExpiryScope::enter() else {
        return;
    };
    wait_for_spawn_to_register();
    kill_active();
    if own_group <= 1 {
        log(&format!(
            "watchdog refuses to kill invalid scanner group {own_group}"
        ));
        return;
    }
    let mut kill = GroupKill {
        killpg: Some(killpg),
        group: own_group,
    };
    let sweep = sweep(own_group);
    kill.fire();
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

/// How long the watchdog waits for a spawn it authorized-then-saw in flight
/// to register its pid; a spawn stalled longer is left to the sweep.
#[cfg_attr(test, allow(dead_code))]
const SPAWN_REGISTER_WAIT: Duration = Duration::from_secs(1);
/// How long the main thread waits for the watchdog's cleanup: the sweep bound
/// plus a margin. In production the final `killpg` ends the process first.
const WATCHDOG_CLEANUP_WAIT: Duration = Duration::from_secs(5);

#[cfg_attr(test, allow(dead_code))]
fn spawn_register_wait() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(SPAWN_REGISTER_WAIT_MS.load(Ordering::SeqCst))
    }
    #[cfg(not(test))]
    {
        SPAWN_REGISTER_WAIT
    }
}

fn wait_for_spawn_to_register() {
    let state = watchdog_state();
    let _ = WATCHDOG_STATE
        .1
        .wait_timeout_while(state, spawn_register_wait(), |s| s.in_flight);
}

/// `Running` -> `Expiring` until dropped, then `Expired`.
struct ExpiryScope;
impl ExpiryScope {
    /// `None` unless the scanner was `Running`: leaving (`Exiting`) or
    /// already expiring/expired scanners are not expired again.
    fn enter() -> Option<Self> {
        let mut state = watchdog_state();
        if state.phase != Phase::Running {
            return None;
        }
        state.phase = Phase::Expiring;
        EXPIRING.store(true, Ordering::SeqCst);
        Some(ExpiryScope)
    }
}
impl Drop for ExpiryScope {
    fn drop(&mut self) {
        watchdog_state().phase = Phase::Expired;
        WATCHDOG_STATE.1.notify_all();
    }
}

/// Call when the main thread leaves (return, `exit`, or a panic via
/// [`ExitGuard`]). `Running` -> `Exiting` (a later watchdog then does nothing)
/// and the leftovers of the primitives that ran (a setsid escapee, a
/// reparented orphan) are swept; mid-expiry, wait (bounded) for the watchdog's
/// cleanup instead, so the process is not ended mid-sweep.
pub fn finish_process() {
    finish_process_with(|| {
        let root = unsafe { libc::getpid() };
        let sweep = crate::containment::kill_descendants(root, DESCENDANT_SWEEP);
        if !sweep.complete || sweep.first_error.is_some() {
            tracing::warn!(
                failures = sweep.failures,
                complete = sweep.complete,
                "exit sweep left descendants"
            );
        }
    });
}

fn finish_process_with(sweep: impl FnOnce()) {
    let mut state = match WATCHDOG_STATE.0.lock() {
        Ok(state) => state,
        Err(poison) => {
            tracing::debug!("watchdog_state lock was poisoned; using its last state");
            poison.into_inner()
        }
    };
    match state.phase {
        Phase::Running => {
            state.phase = Phase::Exiting;
            drop(state);
            sweep();
        }
        Phase::Expiring => {
            let _ = WATCHDOG_STATE
                .1
                .wait_timeout_while(state, WATCHDOG_CLEANUP_WAIT, |s| s.phase == Phase::Expiring);
        }
        Phase::Expired | Phase::Exiting => {}
    }
}

/// Runs [`finish_process`] when dropped, including while a panic unwinds.
pub struct ExitGuard;
impl Drop for ExitGuard {
    fn drop(&mut self) {
        finish_process();
    }
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
/// Test only: pause (ms) after a spawn is authorized and before `spawn()`, and
/// after `spawn()` and before the pid is registered (0: off).
#[cfg(test)]
static DELAY_BEFORE_SPAWN_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static DELAY_REGISTRATION_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Test only: pause (ms) right before `spawn()`, after the last phase check.
#[cfg(test)]
static DELAY_AFTER_CHECK_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Test only: set as soon as `spawn()` has returned, before the pid registers.
#[cfg(test)]
static SPAWN_RETURNED: AtomicBool = AtomicBool::new(false);
/// Test only: the watchdog's wait for an in-flight spawn (ms).
#[cfg(test)]
static SPAWN_REGISTER_WAIT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1000);
/// Test only: the watchdog "fires" right after the next spawn returns, and the
/// spawned pid is recorded here.
#[cfg(test)]
static EXPIRE_AFTER_SPAWN: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SPAWNED_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The invocation watchdog also terminates the active primitive's descendants.
/// Bounded: it never waits more than [`WATCHDOG_LOCK_WAIT`] for the lock. Only
/// the watchdog's `ExpiryScope` moves `Running` to `Expiring`, so a call in any
/// other phase (`Running`, `Exiting`) kills nothing.
fn kill_active_group() {
    let deadline = Instant::now() + WATCHDOG_LOCK_WAIT;
    loop {
        let state = match WATCHDOG_STATE.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            // A primitive registering right now sees the phase and kills itself.
            Err(TryLockError::WouldBlock) => return,
        };
        match state.phase {
            Phase::Running | Phase::Exiting => return,
            Phase::Expiring | Phase::Expired => {}
        }
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
            let mut state = watchdog_state();
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
            // Forked after the watchdog began: never exec (see `EXPIRING`).
            if EXPIRING.load(Ordering::SeqCst) {
                libc::_exit(127);
            }
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
        // Authorized only while `Running`, atomically with marking it in flight.
        let mut state = watchdog_state();
        if state.phase != Phase::Running {
            return Err(expired());
        }
        if state.pid != 0 || state.in_flight {
            return Err(io::Error::other("another system primitive is active"));
        }
        state.in_flight = true;
    }
    #[cfg(test)]
    std::thread::sleep(Duration::from_millis(
        DELAY_BEFORE_SPAWN_MS.load(Ordering::SeqCst),
    ));
    if EXPIRING.load(Ordering::SeqCst) {
        // Resumed after the watchdog began (and maybe finished): do not spawn.
        watchdog_state().in_flight = false;
        WATCHDOG_STATE.1.notify_all();
        return Err(expired());
    }
    #[cfg(test)]
    std::thread::sleep(Duration::from_millis(
        DELAY_AFTER_CHECK_MS.load(Ordering::SeqCst),
    ));
    // Unlocked: a stalled exec handshake must not hold off the watchdog.
    let spawned = command.spawn();
    #[cfg(test)]
    SPAWN_RETURNED.store(true, Ordering::SeqCst);
    #[cfg(test)]
    if DELAY_REGISTRATION.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(60));
    }
    #[cfg(test)]
    std::thread::sleep(Duration::from_millis(
        DELAY_REGISTRATION_MS.load(Ordering::SeqCst),
    ));
    let mut state = watchdog_state();
    state.in_flight = false;
    WATCHDOG_STATE.1.notify_all();
    let child = spawned?;
    // Unreaped, so this pid (and group) stays reserved until Guard clears it.
    state.pid = child.id() as i32;
    #[cfg(test)]
    if EXPIRE_AFTER_SPAWN.swap(false, Ordering::SeqCst) {
        SPAWNED_PID.store(state.pid, Ordering::SeqCst);
        state.phase = Phase::Expiring;
    }
    let expired_meanwhile = state.phase != Phase::Running;
    drop(state);
    let mut child = Guard(child);
    if expired_meanwhile {
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
        let _lock = serial();
        let _scope = ExpiryScope::enter().unwrap();
        kill_active_group();
        assert_eq!(
            fixture("exit 0").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(active_pid(), 0);
    }
    #[test]
    fn expected_rpm_ownership_miss_and_normal_reaping() {
        let _lock = serial();
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
        assert_eq!(active_pid(), 0);
    }
    #[test]
    fn rejects_status_and_misleading_output() {
        let _lock = serial();
        assert!(fixture("exit 1").is_err());
        assert!(fixture("printf 'SecureBoot disabled'; exit 1").is_err());
        assert!(fixture("printf 'not owned by any package'; exit 1").is_err());
        assert!(fixture("printf 'INFECTED'; exit 1").is_err());
        assert_eq!(fixture("printf 'INFECTED'").unwrap(), "INFECTED");
    }
    #[test]
    fn rejects_invalid_utf8_and_missing_primitive() {
        let _lock = serial();
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
                expire_watchdog(own, || {});
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
                expire_watchdog(own, || {});
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
            expire_watchdog(own, || {});
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

    #[test]
    fn main_waits_for_the_watchdog_to_finish_its_sweep() {
        main_exit_fixture_test("return");
    }

    /// The main thread must not return (and so end the process, and the
    /// watchdog thread with it) while the watchdog is still cleaning up: the
    /// watchdog's first step releases `capture`, whose error would otherwise
    /// let `main` exit with a detached descendant still alive. The injected
    /// delay after the release makes the unsynchronised exit certain. `mode`:
    /// how the fixture's main thread leaves ("return": the explicit exit
    /// call; "panic": a panic unwinding past an `ExitGuard`).
    fn main_exit_fixture_test(mode: &str) {
        use std::os::unix::process::ExitStatusExt;
        const CHILD: &str = "SNITCHWATCH_TEST_MAIN_EXITS_EARLY";
        const MODE: &str = "SNITCHWATCH_TEST_MAIN_EXITS_EARLY_MODE";
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
            std::fs::write(
                &file,
                format!(
                    "{}
",
                    detached.id()
                ),
            )
            .unwrap();
            std::thread::spawn(move || {
                // Fire once the primitive below is registered.
                while active_pid() == 0 {
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
            let run = || {
                // Released by the watchdog's first step, with an error.
                let _ = capture(
                    Command::new("/usr/bin/sh").args(["-c", "exec sleep 30"]),
                    Duration::from_secs(60),
                    None,
                );
            };
            if std::env::var(MODE).unwrap() == "panic" {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = ExitGuard;
                    run();
                    panic!("run() failed");
                }));
            } else {
                run();
                finish_process();
            }
            std::process::exit(0);
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!(
                    "command::tests::{}",
                    if mode == "panic" {
                        "the_exit_guard_waits_for_the_watchdog_when_main_panics"
                    } else {
                        "main_waits_for_the_watchdog_to_finish_its_sweep"
                    }
                ),
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .env(MODE, mode)
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
        assert!(exited, "{mode}: the scanner never exited");
        assert_eq!(
            reported.len(),
            1,
            "{mode}: the fixture never reported its pid"
        );
        assert!(
            survivors.is_empty(),
            "{mode}: main exited before the watchdog's sweep: {survivors:?} survived ({status:?})"
        );
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{mode}: {status:?}");
    }

    /// `spawn_in_flight`: a spawn is authorized but its pid not registered.
    fn spawn_in_flight() -> bool {
        watchdog_state().in_flight
    }
    /// `main` leaving, without the real sweep (which would reach other tests'
    /// children in this process).
    fn begin_exit() {
        finish_process_with(|| {});
    }

    /// A spawn authorized before the watchdog fires (paused before `spawn()`,
    /// so the sweep finds nothing) whose primitive then forks a setsid()
    /// descendant before it registers: the watchdog must wait for the
    /// registration, so its kill and sweep see that descendant.
    #[test]
    fn a_spawn_authorized_before_expiry_cannot_escape_the_sweep() {
        const CHILD: &str = "SNITCHWATCH_TEST_INFLIGHT_SPAWN";
        if let Some(file) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            DELAY_REGISTRATION_MS.store(600, Ordering::SeqCst);
            SPAWN_REGISTER_WAIT_MS.store(5000, Ordering::SeqCst);
            let watchdog = std::thread::spawn(move || {
                while !SPAWN_RETURNED.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                expire_watchdog_with(
                    own,
                    kill_active_group,
                    |root| crate::containment::kill_descendants(root, DESCENDANT_SWEEP),
                    |_| {},
                    |_| {},
                );
            });
            let script = format!(
                "setsid -f sh -c 'echo $$ > {0}; exec sleep 60'; exec sleep 30",
                file.to_str().unwrap()
            );
            let _ = capture(
                Command::new("/usr/bin/sh").args(["-c", &script]),
                Duration::from_secs(60),
                None,
            );
            watchdog.join().unwrap();
            let path = std::path::PathBuf::from(&file);
            let start = Instant::now();
            let mut pids = reported_pids(&path);
            while pids.is_empty() && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
                pids = reported_pids(&path);
            }
            let Some(&escapee) = pids.first() else {
                std::process::exit(4);
            };
            let start = Instant::now();
            while !gone(escapee) && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !gone(escapee) {
                unsafe {
                    libc::kill(escapee, libc::SIGKILL);
                }
                std::process::exit(3);
            }
            std::process::exit(0);
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pid");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::a_spawn_authorized_before_expiry_cannot_escape_the_sweep",
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let exited = exited_within(&scanner, Duration::from_secs(20));
        for pid in reported_pids(&pids) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        assert!(exited, "the fixture never finished");
        assert_eq!(
            status.code(),
            Some(0),
            "3: the descendant escaped the sweep; 4: it never started ({status:?})"
        );
    }

    /// Once the process is leaving, a late watchdog does nothing: exit and
    /// expiry are mutually exclusive.
    #[test]
    fn a_watchdog_that_fires_after_exit_began_does_nothing() {
        let _lock = serial();
        begin_exit();
        let events = std::cell::RefCell::new(Vec::new());
        expire_watchdog_with(
            4242,
            || events.borrow_mut().push("active"),
            |_| {
                events.borrow_mut().push("sweep");
                sweep_with(0)
            },
            |_| events.borrow_mut().push("killpg"),
            |_| events.borrow_mut().push("log"),
        );
        let events = events.into_inner();
        reset_watchdog_state();
        assert!(events.is_empty(), "{events:?}");
    }

    /// A panic unwinding out of `run()` takes the guard's exit path too.
    #[test]
    fn the_exit_guard_waits_for_the_watchdog_when_main_panics() {
        main_exit_fixture_test("panic");
    }

    /// A normal exit sweeps the leftovers of a successful primitive: a setsid
    /// escapee must not outlive the scanner.
    #[test]
    fn a_clean_exit_sweeps_a_successful_primitives_escapee() {
        const CHILD: &str = "SNITCHWATCH_TEST_CLEAN_EXIT";
        if let Some(file) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            let script = format!(
                "setsid -f sh -c 'echo $$ > {0}; exec sleep 60' >/dev/null 2>&1",
                file.to_str().unwrap()
            );
            capture(
                Command::new("/usr/bin/sh").args(["-c", &script]),
                Duration::from_secs(30),
                None,
            )
            .unwrap();
            let path = std::path::PathBuf::from(&file);
            let start = Instant::now();
            let mut pids = reported_pids(&path);
            while pids.is_empty() && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
                pids = reported_pids(&path);
            }
            let Some(&escapee) = pids.first() else {
                std::process::exit(4);
            };
            finish_process();
            let start = Instant::now();
            while !gone(escapee) && start.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let code = if gone(escapee) { 0 } else { 3 };
            unsafe {
                libc::kill(escapee, libc::SIGKILL);
            }
            std::process::exit(code);
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pid");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "command::tests::a_clean_exit_sweeps_a_successful_primitives_escapee",
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let exited = exited_within(&scanner, Duration::from_secs(20));
        for pid in reported_pids(&pids) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        assert!(exited, "the fixture never finished");
        assert_eq!(
            status.code(),
            Some(0),
            "3: the escapee outlived the exit; 4: it never started ({status:?})"
        );
    }

    /// A panic inside the sweep must not skip the own-group kill.
    #[test]
    fn a_panicking_sweep_still_kills_the_own_group() {
        let _lock = serial();
        let killed = std::cell::Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expire_watchdog_with(
                4242,
                || {},
                |_| panic!("sweep failed"),
                |g| killed.set(g),
                |_| {},
            );
        }));
        reset_watchdog_state();
        assert!(result.is_err());
        assert_eq!(killed.get(), 4242);
    }

    /// A poisoned watchdog_state lock is explicit, not fatal: its last state is
    /// used (nothing blocks under the lock, so it stays coherent).
    #[test]
    fn a_poisoned_watchdog_state_lock_does_not_stop_the_exit_path() {
        let _lock = serial();
        let _ = std::thread::spawn(|| {
            let _held = WATCHDOG_STATE.0.lock().unwrap();
            panic!("poison the watchdog_state lock");
        })
        .join();
        assert!(WATCHDOG_STATE.0.is_poisoned());
        let swept = std::cell::Cell::new(false);
        finish_process_with(|| swept.set(true));
        assert!(swept.get());
        assert_eq!(watchdog_state().phase, Phase::Exiting);
        WATCHDOG_STATE.0.clear_poison();
    }

    /// A capture that was authorized but resumes only after the watchdog's
    /// bounded wait timed out and the sweep finished (descheduled, stalled)
    /// must not spawn: its primitive's setsid() helper would outlive both
    /// the sweep and the scanner-group kill. `mode`: where the capture is
    /// paused ("before_check": before its last phase check; "after_check":
    /// between that check and `fork`, which only the child's own check stops).
    fn late_capture_fixture_test(mode: &str) {
        const CHILD: &str = "SNITCHWATCH_TEST_LATE_CAPTURE";
        const MODE: &str = "SNITCHWATCH_TEST_LATE_CAPTURE_MODE";
        if let Some(file) = std::env::var_os(CHILD) {
            let own = unsafe { libc::getpid() };
            unsafe {
                assert!(libc::setpgid(0, 0) == 0 || libc::getpgrp() == own);
            }
            crate::containment::become_subreaper().unwrap();
            SPAWN_REGISTER_WAIT_MS.store(100, Ordering::SeqCst);
            if std::env::var(MODE).unwrap() == "before_check" {
                DELAY_BEFORE_SPAWN_MS.store(600, Ordering::SeqCst);
            } else {
                DELAY_AFTER_CHECK_MS.store(600, Ordering::SeqCst);
            }
            DELAY_REGISTRATION_MS.store(300, Ordering::SeqCst);
            let watchdog = std::thread::spawn(move || {
                while !spawn_in_flight() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                expire_watchdog_with(
                    own,
                    kill_active_group,
                    |root| crate::containment::kill_descendants(root, DESCENDANT_SWEEP),
                    |_| {},
                    |_| {},
                );
            });
            let script = format!(
                "setsid -f sh -c 'echo $$ > {0}; exec sleep 60'; exec sleep 30",
                file.to_str().unwrap()
            );
            let result = capture(
                Command::new("/usr/bin/sh").args(["-c", &script]),
                Duration::from_secs(60),
                None,
            );
            watchdog.join().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            let pids = reported_pids(std::path::Path::new(&file));
            if let Some(&helper) = pids.first() {
                unsafe {
                    libc::kill(helper, libc::SIGKILL);
                }
                std::process::exit(3);
            }
            std::process::exit(if result.is_err() { 0 } else { 5 });
        }
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pid");
        let mut scanner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("command::tests::a_late_capture_cannot_leave_a_helper_{mode}"),
                "--test-threads=1",
            ])
            .env(CHILD, &pids)
            .env(MODE, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let exited = exited_within(&scanner, Duration::from_secs(20));
        for pid in reported_pids(&pids) {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let status = kill_group_then_reap(&mut scanner);
        assert!(exited, "{mode}: the fixture never finished");
        assert_eq!(
            status.code(),
            Some(0),
            "{mode}: 3: a detached helper escaped; 5: the late capture ran ({status:?})"
        );
    }
    #[test]
    fn a_late_capture_cannot_leave_a_helper_before_check() {
        late_capture_fixture_test("before_check");
    }
    #[test]
    fn a_late_capture_cannot_leave_a_helper_after_check() {
        late_capture_fixture_test("after_check");
    }

    /// Only an expiry that actually began may emit the timeout marker.
    #[test]
    fn a_noop_expiry_does_not_emit_the_timeout_marker() {
        let _lock = serial();
        // Group 1 is refused after the marker point, so nothing is killed.
        let emitted = std::cell::Cell::new(0);
        expire_watchdog(1, || emitted.set(emitted.get() + 1));
        assert_eq!(emitted.get(), 1, "a running scanner's expiry emits it");
        reset_watchdog_state();
        begin_exit();
        expire_watchdog(1, || emitted.set(emitted.get() + 1));
        assert_eq!(emitted.get(), 1, "an exiting scanner's must not");
    }

    /// Only the watchdog's `ExpiryScope` moves `Running` to `Expiring`.
    #[test]
    fn a_direct_kill_of_the_active_group_does_not_wedge_the_phase() {
        let _lock = serial();
        kill_active_group();
        assert_eq!(watchdog_state().phase, Phase::Running);
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
        let _lock = serial();
        assert!(fixture("head -c 1048577 /dev/zero").is_err());
        assert!(fixture("head -c 65537 /dev/zero >&2").is_err());
        let start = Instant::now();
        assert_eq!(
            fixture("sleep 30 & exit 0").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(active_pid(), 0);
    }

    /// A child that is always killed and reaped, even if an assertion panics.
    struct Reaped(Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn active_pid() -> i32 {
        watchdog_state().pid
    }
    /// Back to a fresh, running scanner (tests simulate expiry in-process).
    fn reset_watchdog_state() {
        EXPIRING.store(false, Ordering::SeqCst);
        EXPIRE_AFTER_SPAWN.store(false, Ordering::SeqCst);
        DELAY_REGISTRATION.store(false, Ordering::SeqCst);
        DELAY_BEFORE_SPAWN_MS.store(0, Ordering::SeqCst);
        DELAY_AFTER_CHECK_MS.store(0, Ordering::SeqCst);
        SPAWN_RETURNED.store(false, Ordering::SeqCst);
        DELAY_REGISTRATION_MS.store(0, Ordering::SeqCst);
        SPAWN_REGISTER_WAIT_MS.store(1000, Ordering::SeqCst);
        STALL_FD.store(-1, Ordering::SeqCst);
        *watchdog_state() = WatchdogState {
            phase: Phase::Running,
            in_flight: false,
            pid: 0,
        };
    }
    /// Serializes the tests that use the process-global watchdog_state, and leaves
    /// it fresh before and after (a panicking test must not poison the rest).
    struct Serial {
        _guard: MutexGuard<'static, ()>,
    }
    fn serial() -> Serial {
        let guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        reset_watchdog_state();
        Serial { _guard: guard }
    }
    impl Drop for Serial {
        fn drop(&mut self) {
            reset_watchdog_state();
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
        let _lock = serial();
        for group in [i32::MIN, -1, 0, 1] {
            reset_watchdog_state();
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
    }

    /// Nothing that can block (logging) may run before the final killpg; the
    /// sweep's failures are only reported afterwards.
    #[test]
    fn killpg_comes_before_every_diagnostic() {
        let _lock = serial();
        for failures in [0, 3] {
            reset_watchdog_state();
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
    }

    #[test]
    fn lock_timeout_lets_the_watchdog_move_on_without_killing() {
        let _lock = serial();
        // A registered primitive in its own group, as `capture` makes it.
        let mut victim = Command::new("/usr/bin/sleep");
        victim.arg("30");
        unsafe {
            victim.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut victim = Reaped(victim.spawn().unwrap());
        let _scope = ExpiryScope::enter().unwrap();
        let mut held = watchdog_state();
        held.pid = victim.0.id() as i32;
        let start = Instant::now();
        kill_active_group();
        let waited = start.elapsed();
        drop(held);
        assert!(waited >= WATCHDOG_LOCK_WAIT, "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        assert!(
            victim.0.try_wait().unwrap().is_none(),
            "killed while contended"
        );
        // Control: uncontended, the same call does kill the registered group.
        kill_active_group();
        let start = Instant::now();
        while victim.0.try_wait().unwrap().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "control: not killed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn expiry_during_spawn_kills_the_registered_group() {
        let _lock = serial();
        EXPIRE_AFTER_SPAWN.store(true, Ordering::SeqCst);
        let result = fixture("exec sleep 30");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let pid = SPAWNED_PID.load(Ordering::SeqCst);
        assert!(pid > 1);
        // Guard killed and reaped it: no such process (not even a zombie).
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
        assert_eq!(active_pid(), 0);
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
            let _lock = serial();
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
