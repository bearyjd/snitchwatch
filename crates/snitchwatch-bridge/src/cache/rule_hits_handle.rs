//! The bridge's shared hit counts: [`RuleHits`] behind a lock, the
//! `RuleHits` broadcast, and the saved file (P2.6 Part 1; see
//! [`crate::cache::rule_hits`] for what the counts mean).
//!
//! **Broadcasting.** A ticker ([`RuleHitsHandle::spawn_ticker`]) sends the
//! full state every [`BROADCAST_PERIOD`], and only when it changed since the
//! last send, so a busy system costs one message per period however many
//! pings arrive. Every send and every snapshot answer builds the message and
//! sends it under the state lock, so a snapshot can't be overtaken by an older
//! state (the same rule `prompt_slot` follows).
//!
//! **Saving.** With a file attached ([`Self::attach_file`]) the ticker saves
//! every [`SAVE_PERIOD`] when something changed, and the bridge saves once
//! more on shutdown ([`Self::save_at_stop`], always written, marked as a
//! clean stop, which the next run needs to judge a daemon restart (N3), and
//! the last: later saves are no-ops). A clean stop read at
//! [`Self::attach_file`] is written out of the file at once. Saves are
//! serialised by one lock, and each writes a temp file of its own
//! (`rule_hits_file::save`), so not even another bridge on the same directory
//! shares it. A file that can't be read is left as it is and the counts stay in memory; a save that fails
//! turns `storage.persistent` off with the reason, and a later success turns
//! it back on. Without a file (no state directory) the counts are in memory
//! only and the message says so.
//!
//! **Lock order:** the rule cache, then the state, then the persistence
//! details. `record` and `adopt_snapshot` are called with the cache locked
//! (or take it), so a commit and a ping can't interleave.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use snitchwatch_proto::protocol::Event;
use tokio::sync::broadcast;
use tracing::{error, warn};

use crate::cache::rule_hits::RuleHits;
use crate::cache::rule_hits_file;
use crate::cache::rules::{RulesCache, SharedRulesCache};
use crate::daemon_commands::DaemonTransport;
use crate::ws_messages::{ServerMessage, StorageStatus};

/// The least time between two `RuleHits` broadcasts.
pub const BROADCAST_PERIOD: Duration = Duration::from_secs(5);
/// How often changed counts are saved.
pub const SAVE_PERIOD: Duration = Duration::from_secs(5 * 60);

/// Where the counts are kept, and what clients are told about it.
struct Persistence {
    file: Option<PathBuf>,
    status: StorageStatus,
    /// The state revision the file holds, once it holds one.
    saved_revision: Option<u64>,
    /// Bumped whenever `status` changes, so the ticker re-sends.
    version: u64,
    /// The shutdown save ran: any later save is a no-op.
    stopped: bool,
}

/// Which save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveKind {
    /// The ticker's: written only when something changed.
    Periodic,
    /// At restore, taking a clean stop out of the file: always written.
    Consume,
    /// At shutdown: always written, marked as a clean stop, and the last.
    Stop,
}

struct Inner {
    state: Mutex<RuleHits>,
    persistence: Mutex<Persistence>,
    /// Held for the length of a save.
    saving: Mutex<()>,
    /// `(state revision, persistence version)` of the last broadcast.
    sent: Mutex<(u64, u64)>,
    broadcast: broadcast::Sender<ServerMessage>,
}

#[derive(Clone)]
pub struct RuleHitsHandle {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn memory_only(reason: Option<String>) -> StorageStatus {
    StorageStatus {
        persistent: false,
        reason,
        unreadable: false,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl RuleHitsHandle {
    /// In-memory only until [`Self::attach_file`] or [`Self::set_storage`].
    pub fn new(broadcast: broadcast::Sender<ServerMessage>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::default(),
                persistence: Mutex::new(Persistence {
                    file: None,
                    status: memory_only(None),
                    saved_revision: None,
                    version: 0,
                    stopped: false,
                }),
                saving: Mutex::new(()),
                sent: Mutex::new((0, 0)),
                broadcast,
            }),
        }
    }

    /// Counts the events of a ping that carried statistics, with its
    /// `uptime` and `rule_hits`. `rules` is the bridge's copy of the daemon's
    /// list; it is locked first.
    pub fn record(&self, events: &[Event], uptime: u64, rule_hits: u64, rules: &SharedRulesCache) {
        // Before waiting on any lock: the judgement compares it with the
        // daemon's uptime.
        let now = now_ms();
        let cache = lock(rules);
        lock(&self.inner.state).record(events, uptime, rule_hits, now, |name| cache.contains(name));
    }

    /// A snapshot was committed: call with the cache locked, after it holds
    /// the new list. Never from `withdraw`.
    pub fn adopt_snapshot(&self, cache: &RulesCache) {
        lock(&self.inner.state).adopt_snapshot(now_ms(), |name| cache.contains(name));
    }

    /// Confirmed `DELETE_RULE`s.
    pub fn forget<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        let mut state = lock(&self.inner.state);
        for name in names {
            state.forget(name);
        }
    }

    /// No file: the counts live in memory, and clients are told `status`.
    pub fn set_storage(&self, status: StorageStatus) {
        let mut persistence = lock(&self.inner.persistence);
        persistence.status = status;
        persistence.version += 1;
    }

    /// Keeps the counts in `path`: restores what an earlier run saved there
    /// (the counts wait for the first snapshot). A file that can't be read
    /// is left alone and the counts stay in memory, with the reason shown.
    ///
    /// A clean stop in the file vouches for the run that wrote it, and this
    /// run is about to count hits the file doesn't hold, so it is taken out
    /// of the file at once (kept in memory for the judgement and for a
    /// shutdown before the first ping). Were this run to die before its
    /// first save, the next would otherwise read it as this run's. If it
    /// can't be taken out, the restart is a gap.
    pub fn attach_file(&self, path: PathBuf) {
        let loaded = rule_hits_file::load(&path);
        let mut consume = false;
        let status = match loaded {
            Ok(saved) => {
                if let Some(saved) = saved {
                    consume = saved.stopped_unix_ms.is_some();
                    lock(&self.inner.state).restore(saved, now_ms());
                }
                None
            }
            Err(e) => {
                let reason = format!("rule hit counts file {}: {e}", path.display());
                error!(%reason, "hit counts are kept in memory only");
                Some(reason)
            }
        };
        {
            let mut persistence = lock(&self.inner.persistence);
            persistence.version += 1;
            match status {
                None => {
                    persistence.file = Some(path);
                    persistence.status = StorageStatus {
                        persistent: true,
                        reason: None,
                        unreadable: false,
                    };
                }
                Some(reason) => persistence.status = memory_only(Some(reason)),
            }
        }
        if consume && !self.save(SaveKind::Consume) {
            lock(&self.inner.state).distrust_restart(now_ms());
        }
    }

    /// How the daemon reaches the bridge: a restart is judged from its
    /// counters only on the root-only Unix socket (`cache::rule_hits`, N3).
    /// Call before [`Self::attach_file`].
    pub fn set_daemon_transport(&self, transport: DaemonTransport) {
        lock(&self.inner.state).trust_daemon_counters(transport == DaemonTransport::Unix);
    }

    /// Saves the counts if they changed since the last save. Safe to call
    /// from any thread.
    pub fn save_now(&self) {
        self.save(SaveKind::Periodic);
    }

    /// The save at shutdown: written even when nothing changed, and marked as
    /// a clean stop (`stoppedUnixMs`; `RuleHits::to_saved_at_stop` says when
    /// not). The last one: a save after it (one the ticker had already handed
    /// to `spawn_blocking`) writes nothing.
    pub fn save_at_stop(&self) {
        self.save(SaveKind::Stop);
    }

    /// Whether no write failed.
    fn save(&self, kind: SaveKind) -> bool {
        let _saving = lock(&self.inner.saving);
        let (path, saved_revision, was_persistent) = {
            let mut persistence = lock(&self.inner.persistence);
            if persistence.stopped {
                return true;
            }
            if kind == SaveKind::Stop {
                persistence.stopped = true;
            }
            let Some(path) = persistence.file.clone() else {
                return true;
            };
            (
                path,
                persistence.saved_revision,
                persistence.status.persistent,
            )
        };
        let (saved, revision) = {
            let state = lock(&self.inner.state);
            let saved = if kind == SaveKind::Stop {
                state.to_saved_at_stop(now_ms())
            } else {
                state.to_saved()
            };
            (saved, state.revision())
        };
        let Some(saved) = saved else { return true };
        if kind == SaveKind::Periodic && saved_revision == Some(revision) && was_persistent {
            return true;
        }
        // Not under any other lock: a pinged `record` must not wait on disk.
        let result = rule_hits_file::save(&path, &saved);
        let mut persistence = lock(&self.inner.persistence);
        match result {
            Ok(()) => {
                persistence.saved_revision = Some(revision);
                if !persistence.status.persistent {
                    persistence.status = StorageStatus {
                        persistent: true,
                        reason: None,
                        unreadable: false,
                    };
                    persistence.version += 1;
                }
                true
            }
            Err(e) => {
                let reason = format!(
                    "couldn't save the rule hit counts to {}: {e}",
                    path.display()
                );
                warn!(%reason, "hit counts are not being saved");
                let status = memory_only(Some(reason));
                if persistence.status != status {
                    persistence.status = status;
                    persistence.version += 1;
                }
                false
            }
        }
    }

    /// The message for the current state.
    pub fn message(&self) -> ServerMessage {
        self.with_message(|_, message| message)
    }

    /// Sends the current state on `tx` (the `RequestSnapshot` answer).
    pub fn announce(&self, tx: &broadcast::Sender<ServerMessage>) {
        self.with_message(|_, message| {
            let _ = tx.send(message);
        });
    }

    /// Builds the message under the state lock and hands it over, still
    /// under that lock, together with the stamp it describes.
    fn with_message<R>(&self, then: impl FnOnce((u64, u64), ServerMessage) -> R) -> R {
        let state = lock(&self.inner.state);
        let persistence = lock(&self.inner.persistence);
        let message = ServerMessage::RuleHits {
            since_unix_ms: state.since_unix_ms(),
            lossy: state.is_lossy(),
            last_gap_unix_ms: state.last_gap_unix_ms(),
            storage: persistence.status.clone(),
            hits: state.wire_hits(),
        };
        then((state.revision(), persistence.version), message)
    }

    /// Broadcasts the state if it changed since the last broadcast.
    pub fn flush(&self) {
        let mut sent = lock(&self.inner.sent);
        self.with_message(|stamp, message| {
            if *sent != stamp {
                *sent = stamp;
                let _ = self.inner.broadcast.send(message);
            }
        });
    }

    /// Runs the broadcast and save schedule until the last handle is
    /// dropped. Abort the task, then call [`Self::save_now`], at shutdown.
    /// The state it starts with is not broadcast: no client can have missed
    /// it, and every snapshot answer carries it.
    pub fn spawn_ticker(&self) -> tokio::task::JoinHandle<()> {
        {
            let mut sent = lock(&self.inner.sent);
            self.with_message(|stamp, _| *sent = stamp);
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(BROADCAST_PERIOD);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut until_save = SAVE_PERIOD;
            loop {
                ticks.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let handle = RuleHitsHandle { inner };
                handle.flush();
                until_save = until_save.saturating_sub(BROADCAST_PERIOD);
                if until_save.is_zero() {
                    until_save = SAVE_PERIOD;
                    let saver = handle.clone();
                    if let Err(e) = tokio::task::spawn_blocking(move || saver.save_now()).await {
                        warn!(error = %e, "saving the rule hit counts failed to run");
                    }
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "rule_hits_handle/tests.rs"]
mod tests;
