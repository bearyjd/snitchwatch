//! NetworkManager-backed source of the "current network identity" used to
//! auto-activate profiles.
//!
//! The [`NetworkWatcher`] trait is the seam: [`NetworkManagerWatcher`] is the
//! real D-Bus-backed implementation, [`NoopNetworkWatcher`] is what
//! [`connect_watcher`] falls back to when NetworkManager (or D-Bus itself)
//! isn't reachable, and tests use their own fake (see
//! [`crate::profiles::test_helpers::FakeNetworkWatcher`]) — none of this
//! module's D-Bus plumbing needs to run for [`crate::profiles::ProfilesManager`]
//! to be exercised.
//!
//! "Current network identity" is NetworkManager's primary active
//! connection's `Id` property (`org.freedesktop.NetworkManager.Connection.Active.Id`),
//! not the raw Wi-Fi SSID bytes. For Wi-Fi connections NetworkManager
//! defaults `Id` to the SSID, and it's also what `nmcli`/`plasma-nm` show as
//! the connection's name (including for wired/VPN connections, which have no
//! SSID at all) — so matching against it covers both "SSID-style" and
//! "connection-name-style" profile matchers from a single property, with no
//! extra D-Bus round trip to the device/wireless/access-point objects.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;
use tracing::{info, warn};

const NM_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// How long after poll-loop start a closed watch channel is still attributed
/// to startup ordering (subscribers not yet attached) rather than teardown.
const STARTUP_GRACE: Duration = Duration::from_secs(30);
const NM_BUS_NAME: &str = "org.freedesktop.NetworkManager";
const NM_OBJECT_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_INTERFACE: &str = "org.freedesktop.NetworkManager";
const NM_ACTIVE_CONNECTION_INTERFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";

/// Source of the current network identity, and a way to be notified when it
/// changes. Implementations must never panic and must degrade to reporting
/// `None` (no known active connection) rather than erroring once started.
#[async_trait]
pub trait NetworkWatcher: Send + Sync + 'static {
    /// The current network identity (NetworkManager's active-connection
    /// `Id`), or `None` if there is no active connection or it's unknown.
    async fn current_connection_id(&self) -> Option<String>;

    /// Subscribe to changes. Fires on every observed transition, including
    /// `None -> Some` and `Some -> None`.
    fn subscribe(&self) -> watch::Receiver<Option<String>>;
}

/// Real NetworkManager-backed watcher. Polls `PrimaryConnection` + its
/// `Id` on an interval rather than subscribing to D-Bus `PropertiesChanged`
/// signals — simpler and just as correct for this use case (profile
/// switching reacting a few seconds late is a non-issue), and avoids
/// depending on exact zbus signal-stream plumbing for a background feature.
pub struct NetworkManagerWatcher {
    tx: watch::Sender<Option<String>>,
}

impl NetworkManagerWatcher {
    /// Connect to the system bus and start polling. Fails if the system bus
    /// or NetworkManager itself is unreachable — callers should fall back to
    /// [`NoopNetworkWatcher`] (see [`connect_watcher`]) rather than treat
    /// this as fatal.
    pub async fn connect() -> zbus::Result<Arc<Self>> {
        let conn = zbus::Connection::system().await?;
        // Best-effort initial read; a transient failure here just starts us
        // at `None` — the poll loop will pick up the real state shortly.
        let initial = read_current_connection_id(&conn).await.unwrap_or(None);
        let (tx, _rx) = watch::channel(initial);
        let watcher = Arc::new(Self { tx });
        watcher.clone().spawn_poll_loop(conn);
        Ok(watcher)
    }

    fn spawn_poll_loop(self: Arc<Self>, conn: zbus::Connection) {
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let mut interval = tokio::time::interval(NM_POLL_INTERVAL);
            loop {
                interval.tick().await;
                match read_current_connection_id(&conn).await {
                    Ok(id) => {
                        if *self.tx.borrow() != id {
                            // `send_replace`, not `send`: `connect()` drops its
                            // initial receiver and `ProfilesManager`'s
                            // auto-switch task subscribes *asynchronously
                            // later*, so an early network change could observe
                            // zero receivers. A plain `send` errors then, and
                            // treating that as "nobody will ever listen" would
                            // permanently kill auto-activation before it began.
                            self.tx.send_replace(id);
                        }
                        // Only treat a closed channel as teardown once the
                        // startup window has safely passed — after that, every
                        // receiver dropping really does mean the consumers are
                        // gone and the poll loop should stop.
                        if self.tx.is_closed() && started.elapsed() > STARTUP_GRACE {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "profiles: NetworkManager poll failed; keeping last known network identity");
                    }
                }
            }
        });
    }
}

#[async_trait]
impl NetworkWatcher for NetworkManagerWatcher {
    async fn current_connection_id(&self) -> Option<String> {
        self.tx.borrow().clone()
    }

    fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.tx.subscribe()
    }
}

async fn read_current_connection_id(conn: &zbus::Connection) -> zbus::Result<Option<String>> {
    let nm_proxy = zbus::Proxy::new(conn, NM_BUS_NAME, NM_OBJECT_PATH, NM_INTERFACE).await?;
    let primary: zbus::zvariant::OwnedObjectPath =
        nm_proxy.get_property("PrimaryConnection").await?;
    // NetworkManager reports "/" (the root path) as its "no object" sentinel.
    if primary.as_str() == "/" {
        return Ok(None);
    }
    let active_proxy = zbus::Proxy::new(
        conn,
        NM_BUS_NAME,
        primary.as_str().to_owned(),
        NM_ACTIVE_CONNECTION_INTERFACE,
    )
    .await?;
    let id: String = active_proxy.get_property("Id").await?;
    Ok(Some(id))
}

/// Fallback watcher used when NetworkManager/D-Bus isn't reachable at all.
/// Always reports `None`, so profile auto-activation simply never fires —
/// manual [`ProfilesManager::activate`] calls keep working unaffected.
#[derive(Default)]
pub struct NoopNetworkWatcher {
    tx: watch::Sender<Option<String>>,
}

impl NoopNetworkWatcher {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(None);
        Self { tx }
    }
}

#[async_trait]
impl NetworkWatcher for NoopNetworkWatcher {
    async fn current_connection_id(&self) -> Option<String> {
        None
    }

    fn subscribe(&self) -> watch::Receiver<Option<String>> {
        self.tx.subscribe()
    }
}

static LOGGED_UNAVAILABLE_ONCE: AtomicBool = AtomicBool::new(false);

/// Connect to NetworkManager, falling back to [`NoopNetworkWatcher`] (and
/// logging exactly once, not on every retry/reconnect elsewhere in the
/// process) if it — or D-Bus itself — isn't reachable. Never panics; this is
/// the only entry point [`crate::profiles::ProfilesManager`] callers should
/// use to obtain a watcher in production.
pub async fn connect_watcher() -> Arc<dyn NetworkWatcher> {
    match NetworkManagerWatcher::connect().await {
        Ok(w) => w,
        Err(e) => {
            if !LOGGED_UNAVAILABLE_ONCE.swap(true, Ordering::SeqCst) {
                info!(
                    error = %e,
                    "profiles: NetworkManager unavailable; auto-activation disabled (manual-only)"
                );
            }
            Arc::new(NoopNetworkWatcher::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_watcher_always_reports_none() {
        let w = NoopNetworkWatcher::new();
        assert_eq!(w.current_connection_id().await, None);
    }

    #[tokio::test]
    async fn noop_watcher_subscribe_never_fires() {
        let w = NoopNetworkWatcher::new();
        let mut rx = w.subscribe();
        let changed = tokio::time::timeout(Duration::from_millis(50), rx.changed()).await;
        assert!(changed.is_err(), "noop watcher must never signal a change");
    }
}
