//! Where the bridge keeps blocklist subscriptions (issue #45 PR A) and, through
//! [`crate::profile_storage`], profiles (issue #46 Part 1).
//!
//! The state directory is resolved **once**, by [`resolve_storage`], called
//! only from `main.rs` (per-user bridge) and [`run_system`](crate::run_system).
//! In-process callers of [`run`](crate::run) (tests, the Tauri shell) never
//! read the environment and never persist: [`RunOptions::in_process`].
//!
//! A storage problem never stops the bridge, which must stay up to answer
//! prompts: it becomes [`EphemeralReason::Unusable`], is logged at `error!`
//! and is shown to the user through `SetBlocklists.storage` and
//! `SetProfiles.storage`.
//!
//! Blocklists are enforced (issue #45 PR B) only by the **system** bridge
//! with a `Persistent` store: the daemon's rules point at list files under
//! `<state>/blocklists`, which must outlive the bridge process and be
//! writable by nobody but the service account. Without a state directory
//! every list reports "no state directory: <reason>"; a per-user bridge
//! reports [`PER_USER_REASON`].

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use snitchwatch_bridge::blocklists::daemon_sink::DaemonRuleSink;
use snitchwatch_bridge::blocklists::fetcher::BlocklistFetch;
use snitchwatch_bridge::blocklists::list_dir::ListDir;
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::{BlocklistsManager, NoopRuleSink, RuleSink};
use snitchwatch_bridge::cache::rules::SharedRulesCache;
use snitchwatch_bridge::daemon_commands::DaemonCommands;
use snitchwatch_bridge::ws_messages::StorageStatus;
use tracing::{error, info, warn};

/// The only state directory the system bridge accepts (`StateDirectory=` of
/// `snitchwatch-system-bridge.service`).
pub const SYSTEM_STATE_DIR: &str = "/var/lib/snitchwatch";
/// The blocklist database's file name inside the state directory.
pub const BLOCKLIST_DB_FILE: &str = "blocklists.sqlite3";
/// Why a per-user bridge installs no blocklist rule (defined with the other
/// blocklist reasons, which GUIs key warnings on).
pub use snitchwatch_bridge::blocklists::PER_USER_REASON;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EphemeralReason {
    /// An in-process [`run`](crate::run) caller: tests, the Tauri shell.
    InProcess,
    /// Neither `$STATE_DIRECTORY` nor `SNITCHWATCH_STATE_DIR` is set.
    NotConfigured,
    /// A state directory was configured but can't be used.
    Unusable(String),
}

impl std::fmt::Display for EphemeralReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProcess => f.write_str("in-process"),
            Self::NotConfigured => f.write_str("not configured"),
            Self::Unusable(reason) => f.write_str(reason),
        }
    }
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
                unreadable: false,
                persistent: true,
                reason: None,
            },
            Storage::Ephemeral(EphemeralReason::Unusable(reason)) => StorageStatus {
                unreadable: false,
                persistent: false,
                reason: Some(reason.clone()),
            },
            Storage::Ephemeral(_) => StorageStatus {
                unreadable: false,
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
    /// Which bridge this is. Only [`BridgeMode::System`] installs
    /// blocklist rules; only `run_system` (and tests) pass it.
    pub mode: BridgeMode,
}

impl RunOptions {
    /// What every in-process [`run`](crate::run) caller gets: nothing is
    /// persisted, and the production fetcher.
    pub fn in_process() -> Self {
        Self {
            storage: Storage::Ephemeral(EphemeralReason::InProcess),
            blocklist_fetcher: None,
            mode: BridgeMode::User,
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
        Storage::Persistent(dir) => {
            info!(state_dir = %dir.display(), "blocklists and profiles are persisted")
        }
        Storage::Ephemeral(EphemeralReason::Unusable(reason)) => error!(
            %reason,
            "state directory unusable; blocklists and profiles are kept in memory only"
        ),
        Storage::Ephemeral(reason) => warn!(
            ?reason,
            "no state directory; blocklists and profiles are kept in memory only"
        ),
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
    let (euid, egid) = effective_ids();
    resolve_storage_as(state_directory, snitchwatch_state_dir, mode, euid, egid)
}

/// [`resolve_storage_from`] as the user `euid`/`egid`, so tests can play the
/// owner of a directory (a test can't `chown`).
pub(crate) fn resolve_storage_as(
    state_directory: Option<OsString>,
    snitchwatch_state_dir: Option<OsString>,
    mode: BridgeMode,
    euid: u32,
    egid: u32,
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
    let checked = match mode {
        BridgeMode::System => std::fs::symlink_metadata(&canonical)
            .map_err(|e| format!("state directory {}: {e}", canonical.display()))
            .and_then(|meta| {
                let facts = DirFacts {
                    uid: meta.uid(),
                    gid: meta.gid(),
                    mode: meta.mode(),
                };
                check_system_state_dir(&facts, euid, egid)
            }),
        BridgeMode::User => secure_user_state_dir(&canonical, euid),
    };
    match checked {
        Ok(()) => Storage::Persistent(canonical),
        Err(reason) => unusable(reason),
    }
}

/// Ownership and mode of a state directory.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirFacts {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

/// [`SYSTEM_STATE_DIR`] must be the service account's own (`User=`/`Group=`
/// of the unit, which systemd's `StateDirectory=` sets) and mode 0700: root
/// opensnitchd reads the blocklist files under it, so nobody else may write
/// there, and the list names aren't anyone else's business either.
pub(crate) fn check_system_state_dir(
    facts: &DirFacts,
    euid: u32,
    egid: u32,
) -> std::result::Result<(), String> {
    if facts.uid != euid || facts.gid != egid {
        return Err(format!(
            "state directory {SYSTEM_STATE_DIR} is not owned by the service account"
        ));
    }
    if facts.mode & 0o777 != 0o700 {
        return Err(format!(
            "state directory {SYSTEM_STATE_DIR} has mode {:o}, not 700",
            facts.mode & 0o7777
        ));
    }
    Ok(())
}

/// What to do with a per-user state directory, from its facts alone:
/// `Ok(None)`: use it as it is. `Ok(Some(mode))`: it is the bridge user's own
/// but group- or world-writable, so drop that write access (`mode` is the new
/// permission bits, nothing else changed). `Err`: it isn't the bridge user's,
/// which is not ours to change.
///
/// The databases and the SQLite journals beside them live there, and whoever
/// can write to the directory can plant a file SQLite then opens (a FIFO hangs
/// it, a link is followed). A directory that is ours is tightened rather than
/// refused: a umask of 002 makes `~/.local/share/snitchwatch` 0775, and
/// refusing would push those users to memory-only storage. Group and others may
/// still read and enter it, unlike the system directory, which root
/// opensnitchd also reads and which must be exactly 0700.
pub(crate) fn plan_user_state_dir(
    facts: &DirFacts,
    euid: u32,
    dir: &Path,
) -> std::result::Result<Option<u32>, String> {
    if facts.uid != euid {
        return Err(format!(
            "state directory {} is not owned by the user running the bridge",
            dir.display()
        ));
    }
    let permissions = facts.mode & 0o7777;
    Ok((permissions & 0o022 != 0).then_some(permissions & !0o022))
}

/// Check a per-user state directory and, if it is ours but writable by group
/// or others, tighten it. Opened `O_DIRECTORY | O_NOFOLLOW` and inspected and
/// changed through that one handle (`fstat`, `fchmod`), so a symlink swapped
/// in since the path was resolved is refused, not followed, and what is
/// checked is what is changed.
pub(crate) fn secure_user_state_dir(dir: &Path, euid: u32) -> std::result::Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let describe = |e: std::io::Error| format!("state directory {}: {e}", dir.display());
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dir)
        .map_err(describe)?;
    let meta = handle.metadata().map_err(describe)?;
    let facts = DirFacts {
        uid: meta.uid(),
        gid: meta.gid(),
        mode: meta.mode(),
    };
    if let Some(mode) = plan_user_state_dir(&facts, euid, dir)? {
        handle
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|e| {
                format!(
                    "state directory {} can be written by other users and couldn't be \
                     tightened: {e}",
                    dir.display()
                )
            })?;
        info!(
            state_dir = %dir.display(),
            from = format_args!("{:o}", facts.mode & 0o7777),
            to = format_args!("{mode:o}"),
            "dropped group and other write access to the state directory"
        );
    }
    Ok(())
}

fn effective_ids() -> (u32, u32) {
    // SAFETY: geteuid/getegid take no arguments, touch no memory and always
    // succeed.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

fn unusable(reason: String) -> Storage {
    Storage::Ephemeral(EphemeralReason::Unusable(reason))
}

/// What a persistent bridge's blocklist rules are sent through (issue #45).
pub(crate) struct DaemonRules {
    pub commands: DaemonCommands,
    pub rules: SharedRulesCache,
}

/// The bridge's blocklist manager: persisted in `<state>/blocklists.sqlite3`
/// when the store opened `Persistent` (in memory otherwise), and enforced
/// through `daemon` only for the system bridge with a persistent store.
pub(crate) fn build_blocklists_manager(
    options: RunOptions,
    daemon: DaemonRules,
) -> Result<Arc<BlocklistsManager>> {
    let (store, storage) = open_blocklist_store(options.storage)?;
    let sink: Arc<dyn RuleSink> = match &storage {
        Storage::Persistent(_) if options.mode != BridgeMode::System => {
            Arc::new(NoopRuleSink::new(PER_USER_REASON))
        }
        Storage::Persistent(dir) => match ListDir::open(dir) {
            Ok(lists) => Arc::new(DaemonRuleSink::new(lists, daemon.commands, daemon.rules)),
            Err(e) => {
                error!(error = %e, "blocklist folder unusable; blocklists are not enforced");
                Arc::new(NoopRuleSink::new(format!(
                    "Couldn't use the blocklist folder: {e}"
                )))
            }
        },
        Storage::Ephemeral(reason) => {
            Arc::new(NoopRuleSink::new(format!("no state directory: {reason}")))
        }
    };
    let mut manager = BlocklistsManager::new(store)
        .with_storage_status(storage.status())
        .with_rule_sink(sink);
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
