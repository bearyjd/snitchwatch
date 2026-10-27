//! The seen set's writer thread: every save (lock retries, fsyncs) happens
//! here, never on the Qt thread. Saves coalesce, the latest snapshot wins
//! (each one holds everything the session knows). A save that cannot take
//! the lock is not written unlocked: it stays pending and is retried every
//! [`RETRY`], at the next save, and at shutdown.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// How often a pending save is retried.
const RETRY: Duration = Duration::from_secs(2);
/// How long dropping the writer waits for its last save.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1_000);

enum Job {
    Save(HashMap<String, i64>),
    /// Try the pending save now; answer whether nothing is pending after
    /// (sent only by tests, `Writer::sync`).
    #[cfg_attr(not(test), allow(dead_code))]
    Sync(Sender<bool>),
    Shutdown,
}

#[derive(Debug)]
pub(super) struct Writer {
    jobs: Sender<Job>,
    done: Receiver<()>,
}

impl Writer {
    pub(super) fn spawn(path: PathBuf, uid: u32) -> Option<Self> {
        let (jobs, inbox) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("seen-programs".to_string())
            .spawn(move || {
                run(&path, uid, &inbox);
                let _ = done_tx.send(());
            })
            .map_err(|error| tracing::warn!(%error, "seen programs: no writer thread"))
            .ok()?;
        Some(Self { jobs, done })
    }

    pub(super) fn save(&self, snapshot: HashMap<String, i64>) {
        let _ = self.jobs.send(Job::Save(snapshot));
    }

    /// Block until the writer has tried the pending save; whether it is
    /// written (tests).
    #[cfg(test)]
    pub(super) fn sync(&self) -> bool {
        let (tx, rx) = mpsc::channel();
        if self.jobs.send(Job::Sync(tx)).is_err() {
            return false;
        }
        rx.recv().unwrap_or(false)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Shutdown);
        // Bounded: a contended lock never holds up quitting for long.
        let _ = self.done.recv_timeout(SHUTDOWN_WAIT);
    }
}

fn run(path: &std::path::Path, uid: u32, inbox: &Receiver<Job>) {
    let mut pending: Option<HashMap<String, i64>> = None;
    loop {
        let job = if pending.is_some() {
            inbox.recv_timeout(RETRY)
        } else {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let mut replies = Vec::new();
        let mut shutdown = matches!(job, Err(RecvTimeoutError::Disconnected));
        // Coalesce: only the newest queued snapshot is written.
        for job in job
            .into_iter()
            .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
        {
            match job {
                Job::Save(snapshot) => pending = Some(snapshot),
                Job::Sync(reply) => replies.push(reply),
                Job::Shutdown => shutdown = true,
            }
        }
        if let Some(snapshot) = &pending {
            match super::write_merged(path, snapshot, uid) {
                Ok(_) => pending = None,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tracing::debug!("seen programs: lock busy, save kept for a retry");
                }
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "seen programs: not saved");
                    pending = None;
                }
            }
        }
        for reply in replies {
            let _ = reply.send(pending.is_none());
        }
        if shutdown {
            return;
        }
    }
}
