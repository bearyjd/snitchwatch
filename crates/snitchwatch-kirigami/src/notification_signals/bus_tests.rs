//! [`Notice`] on a private D-Bus bus (its own `dbus-daemon`, socket under
//! the worktree's `target/t`): a fake notification server, our listener,
//! and another client that tries to click for us. Skipped, loudly, only
//! when `dbus-daemon` isn't installed; the unit tests of [`classify`] run
//! everywhere.

use super::*;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KEYS: [&str; 3] = ["allow-once", "deny", "review"];
const ACTIONS: [(&str, &str); 2] = [("allow-once", "Allow once"), ("deny", "Deny")];

/// A `dbus-daemon` of our own, killed on drop.
struct PrivateBus {
    daemon: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let daemon_bin = ["/usr/bin/dbus-daemon", "/bin/dbus-daemon"]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file())?;
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
        let daemon = Command::new(daemon_bin)
            .arg(format!("--config-file={}", config.display()))
            .arg("--nofork")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "dbus-daemon didn't start");
            std::thread::sleep(Duration::from_millis(20));
        }
        Some(Self {
            daemon,
            address: format!("unix:path={}", socket.display()),
            _dir: dir,
        })
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    async fn notification_server(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .name(SERVER)
            .unwrap()
            .serve_at(PATH, FakeServer)
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

/// Answers `Notify` with id 7.
struct FakeServer;

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl FakeServer {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        _summary: String,
        _body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        7
    }

    fn close_notification(&self, _id: u32) {}
}

async fn action(from: &Connection, to: Option<&UniqueName<'_>>, key: &str) {
    let to = to.map(|name| BusName::from(name.to_owned()));
    from.emit_signal(to, PATH, INTERFACE, "ActionInvoked", &(7u32, key))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_notification_servers_click_counts() {
    let Some(bus) = PrivateBus::start() else {
        eprintln!("SKIPPED: no dbus-daemon, so no private-bus test of notification signals");
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
        eprintln!("SKIPPED: no dbus-daemon, so no private-bus test of notification signals");
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
        eprintln!("SKIPPED: no dbus-daemon, so no private-bus test of notification signals");
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
