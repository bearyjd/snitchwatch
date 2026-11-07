//! Shared helpers for the headless QML integration probes.
//!
//! Lives in a subdirectory so cargo compiles it as a module of each test
//! binary that declares `mod common;`, rather than as a test binary of its
//! own. Kept deliberately small — only genuinely cross-probe machinery
//! belongs here, not per-probe QML or assertions.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::io::AsRawFd;

/// Redirect fd 2 (stderr) into a fresh tempfile for the duration of `f`,
/// then restore it and return everything written while redirected.
///
/// This is how a QML *runtime* error becomes a Rust-side assertion. Qt's
/// default message handler writes `qWarning`/`qCritical` — including QML JS
/// exceptions, prefixed with the offending document's URL — to stderr via the
/// C `FILE*` stream, which respects fd redirection. A JS error inside a
/// handler or binding does NOT null the engine's root object, so without this
/// a broken click path would pass a root-not-null check unnoticed.
///
/// # Safety contract
/// Uses raw `libc::dup`/`dup2`/`close` on the process-global fd 2. Not
/// thread-safe against other code touching fd 2 concurrently — fine for a
/// single-threaded `#[test]` binary invocation, the same assumption the
/// `QT_QPA_PLATFORM` `env::set_var` calls in these probes already make about
/// process-global state.
pub fn capture_stderr<F: FnOnce()>(f: F) -> String {
    let tmp = tempfile::tempfile().expect("create tempfile for stderr capture");
    let tmp_fd = tmp.as_raw_fd();

    // SAFETY: `dup`/`dup2`/`close` are called with fds this function itself
    // owns or has just duplicated; `saved_stderr` is closed exactly once
    // after being dup2'd back onto fd 2.
    let saved_stderr = unsafe { libc::dup(2) };
    assert!(saved_stderr >= 0, "libc::dup(2) failed");
    // SAFETY: `tmp_fd` is a valid, open fd owned by `tmp` for the duration
    // of this call.
    let rc = unsafe { libc::dup2(tmp_fd, 2) };
    assert!(rc >= 0, "libc::dup2(tmp_fd, 2) failed");

    f();

    // SAFETY: `saved_stderr` is a valid fd duplicated above; restoring it
    // onto fd 2 and then closing the now-unneeded duplicate.
    unsafe {
        libc::dup2(saved_stderr, 2);
        libc::close(saved_stderr);
    }

    let mut tmp = tmp;
    tmp.seek(SeekFrom::Start(0))
        .expect("seek captured-stderr tempfile");
    let mut captured = String::new();
    tmp.read_to_string(&mut captured)
        .expect("read captured-stderr tempfile");
    captured
}

/// Set the offscreen/Basic Qt environment every probe needs, unless the
/// caller already chose one.
///
/// `QT_FORCE_STDERR_LOGGING` is **required for [`capture_stderr`] to see
/// anything**, and is the reason this helper exists rather than each probe
/// setting the variables inline. Qt on Fedora is built with journald support,
/// and its default message handler routes to the journal instead of stderr
/// whenever stderr is not a TTY — which is exactly the case under
/// `cargo test`, where it is a pipe. Without this, every `qWarning`,
/// `qCritical`, and QML JS exception vanishes into the journal, the capture
/// comes back empty, and any assertion built on it passes unconditionally.
///
/// Use `QT_FORCE_STDERR_LOGGING`, not the older `QT_LOGGING_TO_CONSOLE`: Qt
/// 6.10 warns that the latter is deprecated. Since this variable is what
/// keeps the stderr assertions load-bearing, a silent removal in a future Qt
/// would reintroduce exactly the vacuous-assertion bug it was added to fix.
pub fn init_headless_qt_env() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }
    if std::env::var_os("QT_FORCE_STDERR_LOGGING").is_none() {
        std::env::set_var("QT_FORCE_STDERR_LOGGING", "1");
    }
}

/// Env var holding the HOME the process was started with (set by the
/// constructor below, before `main`).
#[allow(dead_code)] // only home_isolation() reads it
const ORIGINAL_HOME_VAR: &str = "SNITCHWATCH_PROBE_ORIGINAL_HOME";
/// Env var holding the throwaway home (removed at exit).
const PROBE_HOME_VAR: &str = "SNITCHWATCH_PROBE_HOME";

/// The HOME the process started with, and the throwaway home every test binary
/// that declares `mod common;` runs under instead.
///
/// Probes run the app's own startup in-process (settings, learned list, bridge
/// socket), which reads and writes `~/.config/snitchwatch`; the constructor
/// below points HOME and the XDG dirs at a per-process directory so a plain
/// `cargo test` never touches the developer's own files.
#[allow(dead_code)] // only the lib test that checks the isolation calls it
pub fn home_isolation() -> (Option<std::ffi::OsString>, std::path::PathBuf) {
    (
        std::env::var_os(ORIGINAL_HOME_VAR),
        std::path::PathBuf::from(
            std::env::var_os(PROBE_HOME_VAR).expect(
                "the home-isolation constructor did not run (is this a Linux test binary?)",
            ),
        ),
    )
}

/// What the exit hook may delete: only the directory this process created, and
/// only in the process that created it (a fork child must not delete the
/// parent's home). The env var is never consulted.
#[allow(dead_code)] // used by the Linux exit hook and a lib test
pub fn removal_target(
    retained: Option<&std::path::Path>,
    owner_pid: i32,
    current_pid: i32,
) -> Option<std::path::PathBuf> {
    match retained {
        Some(home) if owner_pid == current_pid => Some(home.to_path_buf()),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
mod isolate {
    use std::ffi::{c_char, CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

    /// The directory this process created (a leaked `CString`), set by the
    /// constructor before `main`; the exit hook deletes this and nothing the
    /// environment says.
    static RETAINED_HOME: AtomicPtr<c_char> = AtomicPtr::new(std::ptr::null_mut());
    /// The pid that created it.
    static OWNER_PID: AtomicI32 = AtomicI32::new(0);

    #[used]
    #[link_section = ".init_array"]
    static ISOLATE_BEFORE_MAIN: extern "C" fn() = isolate_before_main;

    extern "C" fn remove_home() {
        let raw = RETAINED_HOME.load(Ordering::SeqCst);
        // SAFETY: `raw` is null or the leaked, NUL-terminated `CString` stored
        // by the constructor, never freed.
        let retained = (!raw.is_null()).then(|| unsafe { CStr::from_ptr(raw) });
        let retained =
            retained.map(|c| std::path::Path::new(std::ffi::OsStr::from_bytes(c.to_bytes())));
        // SAFETY: getpid has no preconditions.
        let current = unsafe { libc::getpid() };
        if let Some(dir) =
            super::removal_target(retained, OWNER_PID.load(Ordering::SeqCst), current)
        {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// `setenv`, or exit: a failure would leave the developer's real value in
    /// place and the tests running against it.
    unsafe fn set(var: &CStr, value: *const c_char, overwrite: i32) {
        if libc::setenv(var.as_ptr(), value, overwrite) != 0 {
            die(b"cannot set a probe home environment variable\n");
        }
    }

    fn die(message: &[u8]) -> ! {
        // SAFETY: plain write(2) and _exit(2) on a valid buffer.
        unsafe {
            libc::write(2, message.as_ptr().cast(), message.len());
            libc::_exit(1)
        }
    }

    /// `<dir>/<sub>` as a NUL-terminated string in `out`.
    fn join(dir: &[u8], sub: &[u8], out: &mut [u8; 4096]) -> *mut c_char {
        if dir.len() + sub.len() + 2 > out.len() {
            die(b"probe home path too long\n");
        }
        out[..dir.len()].copy_from_slice(dir);
        out[dir.len()] = b'/';
        out[dir.len() + 1..dir.len() + 1 + sub.len()].copy_from_slice(sub);
        out[dir.len() + 1 + sub.len()] = 0;
        out.as_mut_ptr().cast()
    }

    /// Runs from `.init_array` before `main`, while the process is still
    /// single-threaded, so the `setenv` calls cannot race a `getenv`. Only libc
    /// calls on stack buffers, plus one small `Vec` copy of the directory name.
    extern "C" fn isolate_before_main() {
        let mut template = [0u8; 4096];
        // SAFETY: libc calls on NUL-terminated stack buffers; the pointers
        // `getenv` returns are copied before any `setenv`.
        unsafe {
            let tmp = libc::getenv(c"TMPDIR".as_ptr());
            // An empty or relative TMPDIR is treated as unset.
            let tmp = (!tmp.is_null())
                .then(|| CStr::from_ptr(tmp).to_bytes())
                .filter(|dir| dir.first() == Some(&b'/'));
            let dir_ptr = join(
                tmp.unwrap_or(b"/tmp"),
                b"snitchwatch-probe-home-XXXXXX",
                &mut template,
            );
            if libc::mkdtemp(dir_ptr).is_null() {
                die(b"cannot create the probe home\n");
            }
            let dir = CStr::from_ptr(dir_ptr).to_bytes().to_vec();
            if let Ok(retained) = CString::new(dir.clone()) {
                RETAINED_HOME.store(retained.into_raw(), Ordering::SeqCst);
                OWNER_PID.store(libc::getpid(), Ordering::SeqCst);
            }
            let home = libc::getenv(c"HOME".as_ptr());
            if !home.is_null() {
                set(c"SNITCHWATCH_PROBE_ORIGINAL_HOME", home, 1);
            }
            set(c"SNITCHWATCH_PROBE_HOME", dir_ptr, 1);
            set(c"HOME", dir_ptr, 1);
            let mut path = [0u8; 4096];
            for (var, sub) in [
                (c"XDG_CONFIG_HOME", &b".config"[..]),
                (c"XDG_STATE_HOME", b".state"),
                (c"XDG_DATA_HOME", b".data"),
                (c"XDG_RUNTIME_DIR", b"run"),
            ] {
                let p = join(&dir, sub, &mut path);
                // mkdir's mode is masked by the umask; fix it up and check.
                let mut st: libc::stat = std::mem::zeroed();
                if libc::mkdir(p, 0o700) != 0
                    || libc::chmod(p, 0o700) != 0
                    || libc::stat(p, &mut st) != 0
                    || st.st_mode & 0o777 != 0o700
                {
                    die(b"cannot create a 0700 probe home subdirectory\n");
                }
                set(var, p, 1);
            }
            set(c"QT_QPA_PLATFORM", c"offscreen".as_ptr(), 0);
            set(c"QT_QUICK_CONTROLS_STYLE", c"Basic".as_ptr(), 0);
            set(c"QT_FORCE_STDERR_LOGGING", c"1".as_ptr(), 0);
            libc::atexit(remove_home);
        }
    }
}
