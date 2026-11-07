use super::me;
use super::signal as fixture_signal;
use crate::containment::parent_and_zombie;
use std::io;

/// A forked root `P` with a forked `C` below it; both are killed and
/// `P` reaped when dropped, so a failed assertion cannot leak them.
pub(crate) struct ForkedTree {
    pub(crate) root: i32,
    pub(crate) child: i32,
}
impl Drop for ForkedTree {
    fn drop(&mut self) {
        // `C` is not our child: only a live child of `P` is signalled.
        fixture_signal::kill_verified(self.child, |c| {
            parent_and_zombie(c).is_some_and(|(parent, _)| parent == self.root)
        });
        // `P` is our child (a recycled pid is not): only then signal and reap it.
        let sent = fixture_signal::kill_verified(self.root, |r| {
            parent_and_zombie(r).is_some_and(|(parent, _)| parent == me())
        });
        if sent == fixture_signal::Verified::Sent {
            unsafe {
                libc::waitpid(self.root, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// Forks `P`, and `P` forks `C`, which runs `body`. The children never
/// exec, so they close every fd but stdio at once (those would be the
/// pipes of concurrent tests, kept open past their EOF). Only
/// async-signal-safe calls run in the children (forked from a threaded
/// test), `body` included.
pub(crate) fn fork_tree(body: fn()) -> ForkedTree {
    fork_tree_with(body, || unsafe { libc::fork() }).expect("fork_tree")
}

/// [`fork_tree`] with `fork` injected. A failed fork (-1) is an error,
/// never a pid: neither one is ever stored or signalled.
pub(crate) fn fork_tree_with(body: fn(), fork: impl Fn() -> i32) -> io::Result<ForkedTree> {
    let close_inherited = || unsafe {
        libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
    };
    // A child dies with its parent (also if the harness is SIGKILLed), and
    // exits at once if the parent was already gone before the prctl took
    // effect. Async-signal-safe calls only.
    let die_with = |parent: i32| unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 || libc::getppid() != parent {
            libc::_exit(1);
        }
    };
    let mut pipe = [0; 2];
    unsafe {
        assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
        let harness = libc::getpid();
        let root = match fixture_signal::fork_result(fork()) {
            Ok(root) => root,
            Err(e) => {
                libc::close(pipe[0]);
                libc::close(pipe[1]);
                return Err(e);
            }
        };
        if root == 0 {
            die_with(harness);
            let this = libc::getpid();
            // A failed fork is written as -1 and rejected by the reader.
            let child = fork();
            if child == 0 {
                die_with(this);
                close_inherited();
                body();
                loop {
                    libc::pause();
                }
            }
            libc::write(pipe[1], (&child as *const i32).cast(), 4);
            close_inherited();
            loop {
                libc::pause();
            }
        }
        libc::close(pipe[1]);
        let mut child: i32 = 0;
        let read = libc::read(pipe[0], (&mut child as *mut i32).cast(), 4);
        libc::close(pipe[0]);
        let child = if read == 4 {
            fixture_signal::fork_result(child)
        } else {
            Err(io::Error::other("the forked root never reported its child"))
        };
        match child {
            Ok(child) => Ok(ForkedTree { root, child }),
            Err(e) => {
                // Reap the root through the same guarded path.
                drop(ForkedTree { root, child: 0 });
                Err(e)
            }
        }
    }
}
