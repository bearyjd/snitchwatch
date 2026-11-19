//! Strict systemd socket-activation loader for the system bridge mode.
use anyhow::{bail, Context, Result};
use snitchwatch_bridge::daemon_peer::UnixPeerInfo;
use std::collections::HashMap;
use std::os::fd::{FromRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::unix::UCred;
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, warn};

pub const GRPC_SOCKET_PATH: &str = "/run/snitchwatch/opensnitchd.sock";
pub const GUI_SOCKET_PATH: &str = "/run/snitchwatch/bridge.sock";
pub const TOKEN_PATH: &str = "/run/snitchwatch-auth/token";

pub struct ActivatedListeners {
    pub grpc: UnixListener,
    pub gui: UnixListener,
}

pub fn validate_paths(listeners: &ActivatedListeners) -> Result<()> {
    for (listener, expected) in [
        (&listeners.grpc, GRPC_SOCKET_PATH),
        (&listeners.gui, GUI_SOCKET_PATH),
    ] {
        if listener.local_addr()?.as_pathname() != Some(std::path::Path::new(expected)) {
            bail!("activated listener must be bound at {expected}");
        }
    }
    Ok(())
}

pub fn load() -> Result<ActivatedListeners> {
    let pid = std::env::var("LISTEN_PID").context("LISTEN_PID missing")?;
    let count = std::env::var("LISTEN_FDS").context("LISTEN_FDS missing")?;
    let names = std::env::var("LISTEN_FDNAMES").context("LISTEN_FDNAMES missing")?;
    let names = parse_activation(&pid, &count, &names, std::process::id())?;
    let mut found = HashMap::new();
    for (offset, name) in names.into_iter().enumerate() {
        if name != "grpc" && name != "gui" || found.contains_key(&name) {
            bail!("expected uniquely named grpc and gui listeners");
        }
        let listener = duplicate_unix_listener(3 + offset as RawFd)?;
        let expected = if name == "grpc" {
            GRPC_SOCKET_PATH
        } else {
            GUI_SOCKET_PATH
        };
        if listener.local_addr()?.as_pathname() != Some(std::path::Path::new(expected)) {
            bail!("activated {name} listener must be bound at {expected}");
        }
        found.insert(name, listener);
    }
    Ok(ActivatedListeners {
        grpc: found.remove("grpc").unwrap(),
        gui: found.remove("gui").unwrap(),
    })
}

fn parse_activation(pid: &str, count: &str, names: &str, current_pid: u32) -> Result<Vec<String>> {
    if pid.parse::<u32>().context("invalid LISTEN_PID")? != current_pid {
        bail!("LISTEN_PID does not name this process");
    }
    let count = count.parse::<usize>().context("invalid LISTEN_FDS")?;
    if count != 2 {
        bail!("system bridge requires exactly 2 LISTEN_FDS, got {count}");
    }
    let names: Vec<_> = names.split(':').map(str::to_owned).collect();
    if names.len() != count {
        bail!("LISTEN_FDNAMES count does not match LISTEN_FDS");
    }
    if !matches!(names.as_slice(), [grpc, gui] if (grpc == "grpc" && gui == "gui") || (grpc == "gui" && gui == "grpc"))
    {
        bail!("expected uniquely named grpc and gui listeners");
    }
    Ok(names)
}

fn duplicate_unix_listener(fd: RawFd) -> Result<UnixListener> {
    // Duping gives us a CLOEXEC descriptor. The service manager owns the
    // socket path; neither adoption nor shutdown removes it.
    let owned = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 5) };
    if owned < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot dup activated fd");
    }
    let mut domain = 0;
    let mut length = std::mem::size_of_val(&domain) as libc::socklen_t;
    let domain_ok = unsafe {
        libc::getsockopt(
            owned,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            &mut domain as *mut _ as *mut _,
            &mut length,
        )
    };
    let mut socket_type = 0;
    length = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    let type_ok = unsafe {
        libc::getsockopt(
            owned,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut socket_type as *mut _ as *mut _,
            &mut length,
        )
    };
    let mut accept = 0;
    length = std::mem::size_of_val(&accept) as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            owned,
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            &mut accept as *mut _ as *mut _,
            &mut length,
        )
    };
    if domain_ok != 0
        || domain != libc::AF_UNIX
        || type_ok != 0
        || socket_type != libc::SOCK_STREAM
        || ok != 0
        || accept != 1
    {
        unsafe {
            libc::close(owned);
        }
        bail!("activated fd is not a listening Unix stream socket");
    }
    // The service owns inherited descriptors after exec; close those originals
    // only after taking our CLOEXEC duplicate so they cannot leak into children.
    unsafe {
        libc::close(fd);
    }
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(owned) };
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener).context("cannot adopt activated Unix listener")
}

/// Check kernel credentials before tonic sees a daemon connection. Rejected
/// clients are dropped and accepting continues, including after lookup errors.
pub(crate) struct RootUnixIncoming(pub(crate) UnixListener);

impl tokio_stream::Stream for RootUnixIncoming {
    // Plain `UnixStream`, on purpose (#391): every system-socket connection
    // keeps the shared `None` key. The daemon's redial can send Subscribe on
    // one connection and HELLO on the next, and the rule list survives that
    // only because both share a key (plan 2026-11-19-391, section 3). Every
    // peer here is root, so this loses nothing in security.
    type Item = std::io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        poll_accept_filtered(&self.0, cx, |uid| uid == 0)
            .map(|ready| ready.map(|accepted| accepted.map(|(stream, _)| stream)))
    }
}

/// The per-user socket's peer policy (#35): uid 0 (opensnitchd) or the
/// bridge's own uid (dev tools, tests). Every other uid is rejected. The same
/// uid is not a security boundary on Linux: it could ptrace or replace the
/// bridge anyway.
pub(crate) struct OwnerUnixIncoming {
    pub(crate) listener: UnixListener,
    pub(crate) owner_uid: u32,
}

pub(crate) fn owner_peer_allowed(peer_uid: u32, owner_uid: u32) -> bool {
    peer_uid == 0 || peer_uid == owner_uid
}

impl tokio_stream::Stream for OwnerUnixIncoming {
    type Item = std::io::Result<UnixPeerStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let owner_uid = self.owner_uid;
        poll_accept_filtered(&self.listener, cx, |uid| owner_peer_allowed(uid, owner_uid)).map(
            |ready| {
                ready.map(|accepted| {
                    accepted.map(|(stream, cred)| {
                        // After the uid check; the id is the server's, never the peer's.
                        let info = UnixPeerInfo::accept(cred.uid(), cred.pid());
                        debug!(id = info.id(), uid = info.uid(), pid = ?info.pid(), "accepted daemon peer");
                        UnixPeerStream { inner: stream, info }
                    })
                })
            },
        )
    }
}

/// An accepted per-user-socket connection carrying its [`UnixPeerInfo`], which
/// tonic copies into every request's extensions (#391).
pub(crate) struct UnixPeerStream {
    inner: UnixStream,
    info: UnixPeerInfo,
}

impl tonic::transport::server::Connected for UnixPeerStream {
    type ConnectInfo = UnixPeerInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.info
    }
}

impl AsyncRead for UnixPeerStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for UnixPeerStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

fn poll_accept_filtered(
    listener: &UnixListener,
    cx: &mut TaskContext<'_>,
    allow: impl Fn(u32) -> bool,
) -> Poll<Option<std::io::Result<(UnixStream, UCred)>>> {
    // Bound each poll so a flood of rejected clients cannot starve shutdown.
    for _ in 0..32 {
        match listener.poll_accept(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
            Poll::Ready(Ok((stream, _))) => match stream.peer_cred() {
                Ok(cred) if allow(cred.uid()) => return Poll::Ready(Some(Ok((stream, cred)))),
                Ok(cred) => warn!(uid = cred.uid(), "rejected unauthorized daemon peer"),
                Err(error) => {
                    warn!(%error, "rejected daemon peer with unavailable credentials")
                }
            },
        }
    }
    cx.waker().wake_by_ref();
    Poll::Pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn activation_metadata_is_strict_and_accepts_either_fd_order() {
        assert_eq!(
            parse_activation("7", "2", "grpc:gui", 7).unwrap(),
            ["grpc", "gui"]
        );
        assert_eq!(
            parse_activation("7", "2", "gui:grpc", 7).unwrap(),
            ["gui", "grpc"]
        );
        for (pid, count, names) in [
            ("invalid", "2", "grpc:gui"),
            ("8", "2", "grpc:gui"),
            ("7", "invalid", "grpc:gui"),
            ("7", "1", "grpc"),
            ("7", "3", "grpc:gui:other"),
            ("7", "2", "grpc"),
            ("7", "2", "grpc:grpc"),
            ("7", "2", "gui:gui"),
            ("7", "2", "grpc:other"),
            ("7", "2", ":gui"),
        ] {
            assert!(
                parse_activation(pid, count, names, 7).is_err(),
                "{pid}/{count}/{names}"
            );
        }
    }

    #[tokio::test]
    async fn adopted_fd_must_be_a_listening_unix_stream_socket() {
        let dir = tempfile::tempdir().unwrap();
        let std_listener =
            std::os::unix::net::UnixListener::bind(dir.path().join("valid.sock")).unwrap();
        let listener = duplicate_unix_listener(std_listener.into_raw_fd()).unwrap();
        assert_ne!(
            unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        assert_ne!(
            unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
            0
        );
        assert!(listener
            .local_addr()
            .unwrap()
            .as_pathname()
            .unwrap()
            .ends_with("valid.sock"));
        let (unix, _) = std::os::unix::net::UnixStream::pair().unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let regular = std::fs::File::open("/dev/null").unwrap();
        let datagram = std::os::unix::net::UnixDatagram::unbound().unwrap();
        for fd in [
            unix.into_raw_fd(),
            tcp.into_raw_fd(),
            regular.into_raw_fd(),
            datagram.into_raw_fd(),
        ] {
            assert!(duplicate_unix_listener(fd).is_err());
            unsafe {
                libc::close(fd);
            }
        }
        assert!(duplicate_unix_listener(-1).is_err());
    }

    #[test]
    fn owner_policy_allows_root_and_the_owner_only() {
        assert!(owner_peer_allowed(0, 1000));
        assert!(owner_peer_allowed(1000, 1000));
        assert!(!owner_peer_allowed(65534, 1000));
        assert!(!owner_peer_allowed(1001, 1000));
    }

    #[tokio::test]
    async fn owner_incoming_accepts_a_same_uid_peer() {
        use tokio_stream::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grpc.sock");
        let owner_uid = unsafe { libc::geteuid() };
        let mut incoming = OwnerUnixIncoming {
            listener: UnixListener::bind(&path).unwrap(),
            owner_uid,
        };
        let _peer = UnixStream::connect(&path).await.unwrap();
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(2), incoming.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(accepted.info.uid(), owner_uid);
    }

    /// Each accepted per-user connection gets its own, increasing id (#391).
    #[tokio::test]
    async fn owner_incoming_gives_each_connection_its_own_increasing_id() {
        use tokio_stream::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grpc.sock");
        let mut incoming = OwnerUnixIncoming {
            listener: UnixListener::bind(&path).unwrap(),
            owner_uid: unsafe { libc::geteuid() },
        };
        let _peers = [
            UnixStream::connect(&path).await.unwrap(),
            UnixStream::connect(&path).await.unwrap(),
            UnixStream::connect(&path).await.unwrap(),
        ];
        let mut ids = Vec::new();
        for _ in 0..3 {
            let accepted = incoming.next().await.unwrap().unwrap();
            ids.push(tonic::transport::server::Connected::connect_info(&accepted).id());
        }
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    }

    /// The system socket's stream is a plain `UnixStream`: its connect info is
    /// tonic's `UdsConnectInfo`, never a `UnixPeerInfo`, so its peers keep the
    /// shared `None` key (#391).
    #[test]
    fn root_incoming_yields_a_plain_unix_stream() {
        fn item<S, T>(_: &S) -> std::marker::PhantomData<T>
        where
            S: tokio_stream::Stream<Item = std::io::Result<T>>,
        {
            std::marker::PhantomData
        }
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let incoming = RootUnixIncoming(UnixListener::bind(dir.path().join("s.sock")).unwrap());
        let _: std::marker::PhantomData<UnixStream> = item(&incoming);
    }

    #[tokio::test]
    async fn owner_incoming_drops_a_foreign_uid_peer_and_keeps_accepting() {
        use std::os::unix::process::CommandExt;
        use tokio_stream::StreamExt;
        let euid = unsafe { libc::geteuid() };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grpc.sock");
        if euid == 0 {
            // Root peers are always allowed, so the foreign peer is a child
            // re-exec'd as uid 65534 (the helper is in `lib_tests.rs`); the
            // bridge "owner" is a third uid.
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
            let mut incoming = OwnerUnixIncoming {
                listener: UnixListener::bind(&path).unwrap(),
                owner_uid: 1000,
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::non_root_daemon_peer_helper",
                    "--nocapture",
                ])
                .env("SNITCHWATCH_TEST_PEER_SOCKET", &path)
                .uid(65534)
                .gid(65534)
                .spawn()
                .unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(300), incoming.next())
                    .await
                    .is_err(),
                "a foreign-uid peer must not be handed to tonic"
            );
            assert!(child.wait().unwrap().success());
            let _root = UnixStream::connect(&path).await.unwrap();
            let accepted = tokio::time::timeout(std::time::Duration::from_secs(2), incoming.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(accepted.info.uid(), 0);
            return;
        }
        eprintln!("SKIPPED-ROOT-ONLY: ran the non-root fallback; the uid 65534 re-exec needs root");
        // The bridge "owner" is some other uid: our own peer is foreign.
        let mut incoming = OwnerUnixIncoming {
            listener: UnixListener::bind(&path).unwrap(),
            owner_uid: euid + 1,
        };
        let _foreign = UnixStream::connect(&path).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), incoming.next())
                .await
                .is_err(),
            "a foreign-uid peer must not be handed to tonic"
        );
    }

    #[tokio::test]
    async fn activated_listeners_reject_unexpected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let listeners = ActivatedListeners {
            grpc: UnixListener::bind(dir.path().join("grpc.sock")).unwrap(),
            gui: UnixListener::bind(dir.path().join("gui.sock")).unwrap(),
        };
        assert!(validate_paths(&listeners).is_err());
    }
}
