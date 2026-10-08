//! The profile manager's two background tasks: auto-switching on network
//! changes, and the enforcement passes (issue #46 Part 2).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

use super::ProfilesManager;
use crate::profiles::network_watcher::NetworkWatcher;

/// How long a network must stay the same before auto-switching acts on it,
/// so a flapping connection doesn't rewrite firewall rules on every change.
pub const NETWORK_SETTLE: Duration = Duration::from_secs(5);

impl ProfilesManager {
    /// Spawn the auto-switch loop over a live [`NetworkWatcher`]; see
    /// [`spawn_auto_switch_with`](Self::spawn_auto_switch_with).
    pub fn spawn_auto_switch(self: Arc<Self>, watcher: Arc<dyn NetworkWatcher>) -> JoinHandle<()> {
        self.spawn_auto_switch_with(watcher, NETWORK_SETTLE)
    }

    /// Every network value is noted at once ([`Self::note_network`], what a
    /// manual choice is saved with); one that stays the same for `settle`
    /// goes to [`Self::on_network_observed`]. The watcher's value at start
    /// counts too. The loop ends when the watcher does, after acting on its
    /// last value.
    pub fn spawn_auto_switch_with(
        self: Arc<Self>,
        watcher: Arc<dyn NetworkWatcher>,
        settle: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut rx = watcher.subscribe();
            let mut current = watcher.current_connection_id().await;
            loop {
                self.note_network(current.clone());
                let changed = tokio::time::timeout(settle, rx.changed()).await;
                if let Ok(Ok(())) = changed {
                    current = rx.borrow_and_update().clone();
                    continue;
                }
                if let Err(e) = self.on_network_observed(current.clone()).await {
                    warn!(error = %e, "profiles: auto-switch evaluation failed");
                }
                if changed.is_ok() || rx.changed().await.is_err() {
                    // The watcher is gone.
                    return;
                }
                current = rx.borrow_and_update().clone();
            }
        })
    }

    /// Spawn the enforcement loop: one pass now, then one after every
    /// request ([`Self::request_enforcement`], coalesced) and after every
    /// committed daemon rules snapshot (`rules_synced`, taken before the
    /// gRPC server starts so none is missed).
    pub fn spawn_enforcer(
        self: Arc<Self>,
        rules_synced: Option<watch::Receiver<u64>>,
    ) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();
        if let Some(mut synced) = rules_synced {
            let manager = self.clone();
            handles.push(tokio::spawn(async move {
                while synced.changed().await.is_ok() {
                    manager.request_enforcement();
                }
            }));
        }
        handles.push(tokio::spawn(async move {
            loop {
                self.enforce().await;
                self.enforce_requested.notified().await;
            }
        }));
        handles
    }
}
