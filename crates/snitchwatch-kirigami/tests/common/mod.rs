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
