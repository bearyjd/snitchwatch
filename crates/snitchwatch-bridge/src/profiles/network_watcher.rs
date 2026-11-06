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
// clippy 1.99's `double_must_use` fires on async_trait's generated
// `#[must_use]` methods (they return an already-must_use boxed future).
#[allow(clippy::double_must_use)]
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
        let mut reader = NmReader::new(ZbusBus(conn));
        let initial = reader.read().await.unwrap_or(None);
        let (tx, _rx) = watch::channel(initial);
        let watcher = Arc::new(Self { tx });
        watcher.clone().spawn_poll_loop(reader);
        Ok(watcher)
    }

    fn spawn_poll_loop(self: Arc<Self>, mut reader: NmReader<ZbusBus>) {
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let mut interval = tokio::time::interval(NM_POLL_INTERVAL);
            loop {
                interval.tick().await;
                match reader.read().await {
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

/// D-Bus operations the poller needs, abstracted so tests can count proxy
/// builds without a bus.
#[allow(clippy::double_must_use)]
#[async_trait]
trait NmBus: Send + Sync {
    type Proxy: Send + Sync;
    async fn build_proxy(&self, path: &str, interface: &'static str) -> zbus::Result<Self::Proxy>;
    async fn primary_path(&self, nm: &Self::Proxy) -> zbus::Result<String>;
    async fn active_id(&self, active: &Self::Proxy) -> zbus::Result<String>;
}

struct ZbusBus(zbus::Connection);

#[allow(clippy::double_must_use)]
#[async_trait]
impl NmBus for ZbusBus {
    type Proxy = zbus::Proxy<'static>;

    async fn build_proxy(&self, path: &str, interface: &'static str) -> zbus::Result<Self::Proxy> {
        // `CacheProperties::No`: every poll must do a real `Get`, exactly as
        // the previous per-poll proxy did, so a reused proxy can never serve a
        // value that went stale across a NetworkManager restart.
        zbus::proxy::Builder::new(&self.0)
            .destination(NM_BUS_NAME)?
            .path(path.to_owned())?
            .interface(interface)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
    }

    async fn primary_path(&self, nm: &Self::Proxy) -> zbus::Result<String> {
        let p: zbus::zvariant::OwnedObjectPath = nm.get_property("PrimaryConnection").await?;
        Ok(p.as_str().to_owned())
    }

    async fn active_id(&self, active: &Self::Proxy) -> zbus::Result<String> {
        active.get_property("Id").await
    }
}

/// Reads the current connection id, reusing the NetworkManager proxy (and the
/// active-connection proxy while the primary path is unchanged) across polls.
/// Any error drops the cached proxies so the next poll rebuilds them (covers
/// NetworkManager restarting); the 3 s poll interval bounds the retry rate.
struct NmReader<B: NmBus> {
    bus: B,
    nm: Option<B::Proxy>,
    active: Option<(String, B::Proxy)>,
}

impl<B: NmBus> NmReader<B> {
    fn new(bus: B) -> Self {
        Self {
            bus,
            nm: None,
            active: None,
        }
    }

    async fn read(&mut self) -> zbus::Result<Option<String>> {
        let result = self.read_inner().await;
        if result.is_err() {
            self.nm = None;
            self.active = None;
        }
        result
    }

    async fn read_inner(&mut self) -> zbus::Result<Option<String>> {
        if self.nm.is_none() {
            self.nm = Some(self.bus.build_proxy(NM_OBJECT_PATH, NM_INTERFACE).await?);
        }
        let primary = match self.nm.as_ref() {
            Some(nm) => self.bus.primary_path(nm).await?,
            None => return Ok(None),
        };
        // NetworkManager reports "/" (the root path) as its "no object" sentinel.
        if primary == "/" {
            self.active = None;
            return Ok(None);
        }
        if self.active.as_ref().map(|(p, _)| p.as_str()) != Some(primary.as_str()) {
            let proxy = self
                .bus
                .build_proxy(&primary, NM_ACTIVE_CONNECTION_INTERFACE)
                .await?;
            self.active = Some((primary, proxy));
        }
        match self.active.as_ref() {
            Some((_, active)) => Ok(Some(self.bus.active_id(active).await?)),
            None => Ok(None),
        }
    }
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
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct FakeBus {
        builds: AtomicUsize,
        fail_next: AtomicBool,
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl NmBus for Arc<FakeBus> {
        type Proxy = ();
        async fn build_proxy(&self, _: &str, _: &'static str) -> zbus::Result<()> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn primary_path(&self, _: &()) -> zbus::Result<String> {
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(zbus::Error::Failure("nm restarted".into()));
            }
            Ok("/org/freedesktop/NetworkManager/ActiveConnection/1".into())
        }
        async fn active_id(&self, _: &()) -> zbus::Result<String> {
            Ok("home".into())
        }
    }

    #[tokio::test]
    async fn proxies_are_built_once_across_polls_and_rebuilt_after_error() {
        let bus = Arc::new(FakeBus::default());
        let mut reader = NmReader::new(bus.clone());
        for _ in 0..10 {
            assert_eq!(reader.read().await.unwrap(), Some("home".into()));
        }
        // One NM proxy + one active-connection proxy, not 2 per poll.
        assert_eq!(bus.builds.load(Ordering::SeqCst), 2);

        bus.fail_next.store(true, Ordering::SeqCst);
        assert!(reader.read().await.is_err());
        assert_eq!(reader.read().await.unwrap(), Some("home".into()));
        assert_eq!(bus.builds.load(Ordering::SeqCst), 4);
    }

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
