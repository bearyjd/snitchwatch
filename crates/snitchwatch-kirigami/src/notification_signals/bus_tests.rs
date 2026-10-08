//! [`Notice`] on a private D-Bus bus (its own `dbus-daemon`, socket under
//! the worktree's `target/t`): a fake notification server, our listener,
//! and another client that tries to click for us. When `dbus-daemon` isn't
//! installed these are skipped, loudly, except on CI (the `CI` variable is
//! set), where they fail; the unit tests of [`classify`] run everywhere.

use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KEYS: [&str; 3] = ["allow-once", "deny", "review"];
const ACTIONS: [(&str, &str); 2] = [("allow-once", "Allow once"), ("deny", "Deny")];

/// A `dbus-daemon` of our own, killed on drop.
pub(crate) struct PrivateBus {
    daemon: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    /// `None` (skip) when `dbus-daemon` isn't installed, except on CI.
    pub(crate) fn start() -> Option<Self> {
        let Some(daemon_bin) = ["/usr/bin/dbus-daemon", "/bin/dbus-daemon"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file())
        else {
            assert!(
                std::env::var_os("CI").is_none(),
                "no dbus-daemon on CI: the private-bus tests of notification signals must run"
            );
            eprintln!("SKIPPED: no dbus-daemon, so no private-bus test of notification signals");
            return None;
        };
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/t");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::Builder::new()
            .prefix("bus")
            .tempdir_in(base.canonicalize().unwrap())
            .unwrap();
        let socket = dir.path().join("bus");
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<busconfig>\n  <type>session</type>\n  <listen>unix:path={}</listen>\n  \
                 <auth>EXTERNAL</auth>\n  <policy context=\"default\">\n    \
                 <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
                 <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
                socket.display()
            ),
        )
        .unwrap();
        // To a file, not a pipe nobody reads, so a chatty daemon never blocks.
        let log = dir.path().join("stderr");
        let mut daemon = Command::new(daemon_bin)
            .arg(format!("--config-file={}", config.display()))
            .arg("--nofork")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            let exited = daemon.try_wait().unwrap();
            if exited.is_some() || Instant::now() >= deadline {
                let _ = daemon.kill();
                let _ = daemon.wait();
                panic!(
                    "dbus-daemon didn't start ({exited:?}): {}",
                    std::fs::read_to_string(&log).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Some(Self {
            daemon,
            address: format!("unix:path={}", socket.display()),
            _dir: dir,
        })
    }

    pub(crate) async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// A fake notification server. It takes the name with zbus' default
    /// flags, so a later one replaces it.
    pub(crate) async fn notification_server(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .name(SERVER)
            .unwrap()
            .serve_at(PATH, FakeServer::default())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Answers `Notify` with id 7, and counts the calls and closes it gets.
#[derive(Default)]
struct FakeServer {
    notified: u32,
    closed: Vec<u32>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeServer {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        _summary: String,
        _body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        self.notified += 1;
        7
    }

    fn close_notification(&mut self, id: u32) {
        self.closed.push(id);
    }
}

/// How many `Notify` calls fake `server` has answered.
pub(crate) async fn notified(server: &Connection) -> u32 {
    let fake = server
        .object_server()
        .interface::<_, FakeServer>(PATH)
        .await
        .unwrap();
    let count = fake.get().await.notified;
    count
}

/// The ids fake `server` was asked to close.
pub(crate) async fn closed(server: &Connection) -> Vec<u32> {
    let fake = server
        .object_server()
        .interface::<_, FakeServer>(PATH)
        .await
        .unwrap();
    let ids = fake.get().await.closed.clone();
    ids
}

async fn action(from: &Connection, to: Option<&UniqueName<'_>>, key: &str) {
    let to = to.map(|name| BusName::from(name.to_owned()));
    from.emit_signal(to, PATH, INTERFACE, "ActionInvoked", &(7u32, key))
        .await
        .unwrap();
}

/// Returns once the bus has routed everything `conn` sent before: the bus
/// handles one connection's messages in order, so it answers this call
/// only after it has passed on the earlier signals.
async fn routed(conn: &Connection) {
    zbus::fdo::DBusProxy::new(conn)
        .await
        .unwrap()
        .get_id()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_notification_servers_click_counts() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let server = bus.notification_server().await;
    let listener = bus.connect().await;
    let attacker = bus.connect().await;

    let mut notice = Notice::show(&listener, "summary", "body", &ACTIONS)
        .await
        .unwrap();
    let us = listener.unique_name().unwrap().to_owned();
    // Another client clicks for us: broadcast, then sent to us directly.
    action(&attacker, None, "allow-once").await;
    action(&attacker, Some(&us), "allow-once").await;
    // Those reach us first, so a filter that let them through would fail.
    routed(&attacker).await;
    // The server, with a key that isn't ours, then with ours, sent to us
    // the way a server answering the `Notify` caller does.
    action(&server, Some(&us), "default").await;
    action(&server, Some(&us), "deny").await;
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        notice.wait(&KEYS, std::future::pending()),
    )
    .await
    .expect("the server's targeted click never arrived");
    assert_eq!(end, WaitEnd::Action("deny"));

    // A server that broadcasts its signals is heard too.
    let mut notice = Notice::show(&listener, "summary", "body", &ACTIONS)
        .await
        .unwrap();
    action(&attacker, None, "deny").await;
    routed(&attacker).await;
    action(&server, None, "allow-once").await;
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        notice.wait(&KEYS, std::future::pending()),
    )
    .await
    .expect("the server's broadcast click never arrived");
    assert_eq!(end, WaitEnd::Action("allow-once"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_goes_away_voids_the_notice() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let server = bus.notification_server().await;
    let listener = bus.connect().await;
    let mut notice = Notice::show(&listener, "summary", "body", &ACTIONS)
        .await
        .unwrap();
    server.release_name(SERVER).await.unwrap();

    let end = tokio::time::timeout(
        Duration::from_secs(5),
        notice.wait(&KEYS, std::future::pending()),
    )
    .await
    .expect("the owner change never arrived");
    assert_eq!(end, WaitEnd::ServerChanged);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_ends_the_wait() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _server = bus.notification_server().await;
    let listener = bus.connect().await;
    let mut notice = Notice::show(&listener, "summary", "body", &ACTIONS)
        .await
        .unwrap();
    assert_eq!(notice.wait(&KEYS, async {}).await, WaitEnd::Stopped);
    notice.close().await;
}

/// The server is started on demand: it takes the name after `show` began
/// hearing owner changes, so that change (`""` to the server) is already
/// queued when `wait` starts. It isn't the server we showed on going away,
/// so the server's click still counts.
#[tokio::test(flavor = "multi_thread")]
async fn an_on_demand_servers_click_counts() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let listener = bus.connect().await;
    let dbus = DBusProxy::new(&listener).await.unwrap();
    let owner_changes = hear_owner_changes(&dbus).await.unwrap();
    let server = bus.notification_server().await;
    let mut notice = Notice::show_heard(&listener, &dbus, owner_changes, "s", "b", &ACTIONS)
        .await
        .unwrap();
    let us = listener.unique_name().unwrap().to_owned();
    action(&server, Some(&us), "allow-once").await;
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        notice.wait(&KEYS, std::future::pending()),
    )
    .await
    .expect("the on-demand server's click never arrived");
    assert_eq!(end, WaitEnd::Action("allow-once"));
}

/// After that first owner change, another server taking the name over
/// from ours does void the notice: neither server's click counts after it,
/// and the notice is closed on the server that showed it, not the new one
/// (whose notification 7 is somebody else's).
#[tokio::test(flavor = "multi_thread")]
async fn a_later_takeover_voids_an_on_demand_notice() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let listener = bus.connect().await;
    let dbus = DBusProxy::new(&listener).await.unwrap();
    let owner_changes = hear_owner_changes(&dbus).await.unwrap();
    let ours = bus.notification_server().await;
    let mut notice = Notice::show_heard(&listener, &dbus, owner_changes, "s", "b", &ACTIONS)
        .await
        .unwrap();
    let newer = bus.notification_server().await;
    let us = listener.unique_name().unwrap().to_owned();
    action(&ours, Some(&us), "allow-once").await;
    action(&newer, Some(&us), "allow-once").await;
    // Both clicks are on their way, queued behind the takeover.
    routed(&ours).await;
    routed(&newer).await;
    let end = tokio::time::timeout(
        Duration::from_secs(5),
        notice.wait(&KEYS, std::future::pending()),
    )
    .await
    .expect("the takeover never arrived");
    assert_eq!(end, WaitEnd::ServerChanged);

    notice.close().await;
    assert_eq!(closed(&ours).await, [7]);
    assert_eq!(closed(&newer).await, Vec::<u32>::new());
}
