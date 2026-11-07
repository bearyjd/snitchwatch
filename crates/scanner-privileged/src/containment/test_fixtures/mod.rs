//! Test-only fixtures for the containment and command tests.

pub(crate) mod forked_tree;
pub(crate) mod marked_sleeps;
pub(crate) mod reaped;
pub(crate) mod scanner_fixture;
pub(crate) mod signal;

/// The test process's own pid.
pub(crate) fn me() -> i32 {
    unsafe { libc::getpid() }
}
