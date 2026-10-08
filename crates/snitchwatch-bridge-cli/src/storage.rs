//! Where the bridge keeps blocklist subscriptions (issue #45 PR A).
//!
//! The state directory is resolved **once**, by [`resolve_storage`], called
//! only from `main.rs` (per-user bridge) and [`run_system`](crate::run_system).
//! In-process callers of [`run`](crate::run) (tests, the Tauri shell) never
//! read the environment and never persist: [`RunOptions::in_process`].
//!
//! A storage problem never stops the bridge, which must stay up to answer
//! prompts: it becomes [`EphemeralReason::Unusable`], is logged at `error!`
//! and is shown to the user through `SetBlocklists.storage`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use snitchwatch_bridge::blocklists::fetcher::BlocklistFetch;
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::ws_messages::StorageStatus;
use tracing::{error, info, warn};

/// The only state directory the system bridge accepts (`StateDirectory=` of
/// `snitchwatch-system-bridge.service`).
pub const SYSTEM_STATE_DIR: &str = "/var/lib/snitchwatch";
/// The blocklist database's file name inside the state directory.
pub const BLOCKLIST_DB_FILE: &str = "blocklists.sqlite3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralReason {
    /// An in-process [`run`](crate::run) caller: tests, the Tauri shell.
    InProcess,
    /// Neither `$STATE_DIRECTORY` nor `SNITCHWATCH_STATE_DIR` is set.
    NotConfigured,
    /// A state directory was configured but can't be used.
    Unusable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// A canonical, existing state directory.
    Persistent(PathBuf),
    Ephemeral(EphemeralReason),
}

impl Storage {
    /// What GUIs are told. Only an `Unusable` reason is shown to the user.
    pub fn status(&self) -> StorageStatus {
        match self {
            Storage::Persistent(_) => StorageStatus {
                persistent: true,
                reason: None,
            },
            Storage::Ephemeral(EphemeralReason::Unusable(reason)) => StorageStatus {
                persistent: false,
                reason: Some(reason.clone()),
            },
            Storage::Ephemeral(_) => StorageStatus {
                persistent: false,
                reason: None,
            },
        }
    }
}

/// Which bridge is resolving its state directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeMode {
    /// `snitchwatch-bridge.service` (or a manual start).
    User,
    /// `snitchwatch-system-bridge.service`: must resolve to
    /// [`SYSTEM_STATE_DIR`].
    System,
}

/// Options for [`run_with_options`](crate::run_with_options).
pub struct RunOptions {
    pub storage: Storage,
    /// Tests only. `None` means the production https fetcher; `main.rs`
    /// never sets this.
    pub blocklist_fetcher: Option<Arc<dyn BlocklistFetch>>,
}

impl RunOptions {
    /// What every in-process [`run`](crate::run) caller gets: nothing is
    /// persisted, and the production fetcher.
    pub fn in_process() -> Self {
        Self {
            storage: Storage::Ephemeral(EphemeralReason::InProcess),
            blocklist_fetcher: None,
        }
    }
}

/// Resolve the state directory from the environment and log the outcome.
/// Call only from `main.rs` and `run_system`.
pub fn resolve_storage(mode: BridgeMode) -> Storage {
    let storage = resolve_storage_from(
        std::env::var_os("STATE_DIRECTORY"),
        std::env::var_os("SNITCHWATCH_STATE_DIR"),
        mode,
    );
    match &storage {
        Storage::Persistent(dir) => info!(state_dir = %dir.display(), "blocklists are persisted"),
        Storage::Ephemeral(EphemeralReason::Unusable(reason)) => {
            error!(%reason, "state directory unusable; blocklists are kept in memory only")
        }
        Storage::Ephemeral(reason) => {
            warn!(
                ?reason,
                "no state directory; blocklists are kept in memory only"
            )
        }
    }
    storage
}

/// [`resolve_storage`] without the environment: `$STATE_DIRECTORY` (set by
/// systemd's `StateDirectory=`) wins over `SNITCHWATCH_STATE_DIR`; an empty
/// value counts as unset. The path is canonicalized (`/home` is a symlink to
/// `/var/home` on Bazzite, and PR B's daemon rules need a byte-stable path).
pub fn resolve_storage_from(
    state_directory: Option<OsString>,
    snitchwatch_state_dir: Option<OsString>,
    mode: BridgeMode,
) -> Storage {
    let Some(configured) = [state_directory, snitchwatch_state_dir]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
    else {
        return Storage::Ephemeral(EphemeralReason::NotConfigured);
    };
    let configured = PathBuf::from(configured);
    let canonical = match configured.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            return unusable(format!("state directory {}: {e}", configured.display()));
        }
    };
    if !canonical.is_dir() {
        return unusable(format!(
            "state directory {} is not a directory",
            canonical.display()
        ));
    }
    if mode == BridgeMode::System && canonical != Path::new(SYSTEM_STATE_DIR) {
        return unusable(format!(
            "unexpected state directory {}",
            canonical.display()
        ));
    }
    Storage::Persistent(canonical)
}

fn unusable(reason: String) -> Storage {
    Storage::Ephemeral(EphemeralReason::Unusable(reason))
}

/// The bridge's blocklist manager: persisted in `<state>/blocklists.sqlite3`
/// when `options.storage` is `Persistent`, in memory otherwise.
pub(crate) fn build_blocklists_manager(options: RunOptions) -> Result<Arc<BlocklistsManager>> {
    let (store, storage) = open_blocklist_store(options.storage)?;
    let mut manager = BlocklistsManager::new(store).with_storage_status(storage.status());
    if let Some(fetcher) = options.blocklist_fetcher {
        manager = manager.with_fetcher(fetcher);
    }
    Ok(Arc::new(manager))
}

/// Open the store `storage` calls for. A persistent store that fails to open
/// falls back to memory as `Unusable("blocklist store: …")`; only a failing
/// in-memory open is fatal.
fn open_blocklist_store(storage: Storage) -> Result<(Arc<BlocklistStore>, Storage)> {
    let storage = match storage {
        Storage::Persistent(dir) => {
            let path = dir.join(BLOCKLIST_DB_FILE);
            match BlocklistStore::open(&path) {
                Ok(store) => {
                    info!(path = %path.display(), "opened persistent blocklist store");
                    return Ok((Arc::new(store), Storage::Persistent(dir)));
                }
                Err(e) => {
                    let reason = format!("blocklist store: {e}");
                    error!(path = %path.display(), %reason, "blocklists are kept in memory only");
                    unusable(reason)
                }
            }
        }
        ephemeral => ephemeral,
    };
    let store =
        BlocklistStore::open_in_memory().context("failed to open in-memory blocklist store")?;
    Ok((Arc::new(store), storage))
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
