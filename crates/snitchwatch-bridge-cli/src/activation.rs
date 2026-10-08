//! Strict systemd socket-activation loader for the system bridge mode.
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::os::fd::{FromRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use tokio::net::{UnixListener, UnixStream};
use tracing::warn;

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
    type Item = std::io::Result<UnixStream>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        // Bound each poll so a flood of rejected clients cannot starve shutdown.
        for _ in 0..32 {
            match self.0.poll_accept(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Some(Err(error))),
                Poll::Ready(Ok((stream, _))) => match stream.peer_cred() {
                    Ok(cred) if cred.uid() == 0 => return Poll::Ready(Some(Ok(stream))),
                    Ok(cred) => warn!(uid = cred.uid(), "rejected non-root daemon peer"),
                    Err(error) => {
                        warn!(%error, "rejected daemon peer with unavailable credentials")
                    }
                },
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, IntoRawFd};

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
