//! Bridge endpoint and runtime configuration.

use anyhow::{bail, Context, Result};
use snitchwatch_bridge::auth;
use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Env var that allows the dev-only TCP gRPC listener (#35).
pub const DEV_TCP_ENV: &str = "SNITCHWATCH_DEV_TCP";
/// Env var naming the dev-only TCP gRPC bind address.
pub const GRPC_BIND_ENV: &str = "SNITCHWATCH_GRPC_BIND";
/// Directory (a sibling of the WS directory, never inside it) for the
/// per-user gRPC socket.
pub const USER_GRPC_DIR: &str = "snitchwatch-grpc";
/// The per-user gRPC socket's file name (same as the system socket's).
pub const USER_GRPC_SOCKET: &str = "opensnitchd.sock";

/// The actual daemon endpoint. System mode never has a TCP address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrpcEndpoint {
    Tcp(SocketAddr),
    /// The root-only system socket.
    Unix(PathBuf),
    /// The per-user socket; peers are not proven root (#35).
    UserUnix(PathBuf),
}

impl GrpcEndpoint {
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        match self {
            Self::Tcp(addr) => Some(*addr),
            Self::Unix(_) | Self::UserUnix(_) => None,
        }
    }

    pub fn unix_path(&self) -> Option<&Path> {
        match self {
            Self::Unix(path) | Self::UserUnix(path) => Some(path),
            Self::Tcp(_) => None,
        }
    }
}

/// Where the bridge's gRPC `Ui` server listens. TCP is only reachable by
/// naming [`GrpcListen::DevTcp`]; nothing defaults to it (#35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrpcListen {
    /// Loopback TCP, dev/test only. Not deny-safe: any local process that
    /// binds the port first answers for the daemon.
    DevTcp(SocketAddr),
    /// The per-user Unix socket (`$XDG_RUNTIME_DIR/snitchwatch-grpc/`).
    UserUnix(PathBuf),
}

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Where the gRPC `Ui` server listens; opensnitchd dials this. A
    /// `DevTcp` port of `0` is ephemeral.
    pub grpc_listen: GrpcListen,
    /// Path to the Unix domain socket the WS server binds. Defaults to
    /// `$XDG_RUNTIME_DIR/snitchwatch/bridge.sock`.
    pub ws_socket_path: PathBuf,
    /// Cache capacity (number of recent rows retained).
    pub cache_capacity: usize,
}

impl BridgeConfig {
    pub fn from_env() -> Result<Self> {
        let grpc_listen = resolve_grpc_listen(
            std::env::var_os(DEV_TCP_ENV),
            std::env::var_os(GRPC_BIND_ENV),
            std::env::var_os("XDG_RUNTIME_DIR"),
            std::env::var_os("SNITCHWATCH_SYSTEM_BRIDGE"),
        )?;

        let ws_socket_path = std::env::var_os("SNITCHWATCH_WS_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| auth::runtime_dir().join("bridge.sock"));

        Ok(Self {
            grpc_listen,
            ws_socket_path,
            cache_capacity: 10_000,
        })
    }

    /// Loopback TCP on an ephemeral port: the shape every in-process test
    /// used before #35. Test support only.
    #[doc(hidden)]
    pub fn dev_tcp_for_tests(ws_socket_path: PathBuf) -> Self {
        Self {
            grpc_listen: GrpcListen::DevTcp(SocketAddr::from(([127, 0, 0, 1], 0))),
            ws_socket_path,
            cache_capacity: 10_000,
        }
    }

    /// The per-user Unix gRPC socket under `dir` (a fresh tempdir): the shape
    /// `from_env` resolves for `XDG_RUNTIME_DIR=dir`, built without reading
    /// or setting the environment. Test support only.
    #[doc(hidden)]
    pub fn user_unix_for_tests(dir: &Path) -> Self {
        let grpc_path = dir.join(USER_GRPC_DIR).join(USER_GRPC_SOCKET);
        debug_assert!(
            grpc_path.as_os_str().len() < 108,
            "gRPC socket path exceeds sun_path: {}",
            grpc_path.display()
        );
        Self {
            grpc_listen: GrpcListen::UserUnix(grpc_path),
            ws_socket_path: dir.join("snitchwatch").join("bridge.sock"),
            cache_capacity: 10_000,
        }
    }

    /// The system bridge's configuration: its sockets are activated, never
    /// bound, so there is no listen choice to make. `grpc_listen` is a
    /// placeholder that `run_system` ignores.
    pub(crate) fn system() -> Self {
        Self {
            grpc_listen: GrpcListen::UserUnix(PathBuf::from(crate::activation::GRPC_SOCKET_PATH)),
            ws_socket_path: PathBuf::from(crate::activation::GUI_SOCKET_PATH),
            cache_capacity: 10_000,
        }
    }
}

/// Pure over its inputs (no environment access) so it is unit-testable
/// without `set_var`. See `docs/superpowers/plans/2026-11-16-35-tcp-dev-only.md`.
pub(crate) fn resolve_grpc_listen(
    dev_flag: Option<OsString>,
    grpc_bind: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    system_flag: Option<OsString>,
) -> Result<GrpcListen> {
    if system_flag.as_deref() == Some(OsStr::new("1")) {
        reject_dev_tcp_in_system_mode(dev_flag.as_deref())?;
    }
    let dev_tcp = match dev_flag.as_deref() {
        None => false,
        Some(v) if v == OsStr::new("1") => true,
        Some(_) => bail!("{DEV_TCP_ENV} must be 1 when set"),
    };
    if dev_tcp {
        return resolve_dev_tcp(grpc_bind);
    }
    if grpc_bind.is_some() {
        bail!(
            "{GRPC_BIND_ENV} is set but TCP is dev-only: set {DEV_TCP_ENV}=1 to use it, or \
             remove {GRPC_BIND_ENV} (a stale unit or script?) to use the per-user Unix socket"
        );
    }
    let Some(xdg) = xdg_runtime_dir.filter(|dir| !dir.is_empty()) else {
        bail!(
            "XDG_RUNTIME_DIR is not set: the per-user gRPC socket needs it \
             (or set {DEV_TCP_ENV}=1 for loopback TCP, dev only)"
        );
    };
    if !Path::new(&xdg).is_absolute() {
        bail!(
            "XDG_RUNTIME_DIR must be an absolute path (got {xdg:?}): a relative path would \
             put the per-user gRPC socket under the current directory"
        );
    }
    Ok(GrpcListen::UserUnix(
        PathBuf::from(xdg)
            .join(USER_GRPC_DIR)
            .join(USER_GRPC_SOCKET),
    ))
}

fn resolve_dev_tcp(grpc_bind: Option<OsString>) -> Result<GrpcListen> {
    let bind = match grpc_bind {
        None => "127.0.0.1:0".to_string(),
        Some(raw) => raw
            .into_string()
            .map_err(|raw| anyhow::anyhow!("invalid {GRPC_BIND_ENV}: {raw:?}"))?,
    };
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid {GRPC_BIND_ENV}: {bind}"))?;
    if !addr.ip().is_loopback() {
        bail!("{GRPC_BIND_ENV} must be a loopback address (got {addr}); {DEV_TCP_ENV} never exposes gRPC beyond loopback");
    }
    Ok(GrpcListen::DevTcp(addr))
}

/// System mode never has TCP: reject the dev flag outright (any value).
pub fn reject_dev_tcp_in_system_mode(dev_flag: Option<&OsStr>) -> Result<()> {
    if dev_flag.is_some() {
        bail!("{DEV_TCP_ENV} cannot be combined with SNITCHWATCH_SYSTEM_BRIDGE=1: the system bridge never listens on TCP");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn user_unix_for_tests_matches_the_from_env_shape_under_dir() {
        let dir = Path::new("/tmp/sw-x");
        let cfg = BridgeConfig::user_unix_for_tests(dir);
        // Same path `from_env` resolves for XDG_RUNTIME_DIR=dir.
        let resolved =
            resolve_grpc_listen(None, None, Some(dir.as_os_str().to_owned()), None).unwrap();
        assert_eq!(cfg.grpc_listen, resolved);
        assert_eq!(
            cfg.grpc_listen,
            GrpcListen::UserUnix(dir.join("snitchwatch-grpc").join("opensnitchd.sock"))
        );
        assert_eq!(
            cfg.ws_socket_path,
            dir.join("snitchwatch").join("bridge.sock")
        );
        assert_eq!(cfg.cache_capacity, 10_000);
    }

    #[test]
    fn user_unix_for_tests_grpc_path_fits_sun_path() {
        let dir = tempfile::tempdir().unwrap();
        let GrpcListen::UserUnix(path) = BridgeConfig::user_unix_for_tests(dir.path()).grpc_listen
        else {
            panic!("not a UserUnix listener");
        };
        assert!(path.as_os_str().len() < 108, "{}", path.display());
    }

    #[test]
    fn unix_path_is_some_for_both_unix_flavours_and_none_for_tcp() {
        let p = PathBuf::from("/run/x/opensnitchd.sock");
        assert_eq!(GrpcEndpoint::Unix(p.clone()).unix_path(), Some(p.as_path()));
        assert_eq!(
            GrpcEndpoint::UserUnix(p.clone()).unix_path(),
            Some(p.as_path())
        );
        let tcp = GrpcEndpoint::Tcp(SocketAddr::from(([127, 0, 0, 1], 1)));
        assert_eq!(tcp.unix_path(), None);
    }

    #[test]
    fn unset_flag_gives_the_per_user_socket_under_snitchwatch_grpc() {
        let got = resolve_grpc_listen(None, None, os("/run/user/1000"), None).unwrap();
        assert_eq!(
            got,
            GrpcListen::UserUnix("/run/user/1000/snitchwatch-grpc/opensnitchd.sock".into())
        );
    }

    #[test]
    fn user_socket_is_not_inside_the_ws_directory() {
        let GrpcListen::UserUnix(path) =
            resolve_grpc_listen(None, None, os("/run/user/1000"), None).unwrap()
        else {
            panic!("expected UserUnix");
        };
        assert!(!path.starts_with("/run/user/1000/snitchwatch/"));
    }

    #[test]
    fn flag_alone_gives_ephemeral_loopback_tcp() {
        let got = resolve_grpc_listen(os("1"), None, None, None).unwrap();
        assert_eq!(got, GrpcListen::DevTcp("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn flag_with_loopback_bind_is_accepted() {
        for bind in ["127.0.0.1:50051", "[::1]:0"] {
            let got = resolve_grpc_listen(os("1"), os(bind), None, None).unwrap();
            assert_eq!(got, GrpcListen::DevTcp(bind.parse().unwrap()));
        }
    }

    #[test]
    fn flag_with_a_non_loopback_bind_is_an_error() {
        for bind in ["0.0.0.0:50051", "192.168.1.5:50051", "[::]:0"] {
            assert!(resolve_grpc_listen(os("1"), os(bind), None, None).is_err());
        }
    }

    #[test]
    fn flag_with_an_unparsable_bind_is_an_error() {
        assert!(resolve_grpc_listen(os("1"), os("not-an-address"), None, None).is_err());
    }

    #[test]
    fn a_flag_value_other_than_1_is_an_error() {
        for value in ["0", "true", "", "yes"] {
            let err = resolve_grpc_listen(os(value), None, os("/run/user/1000"), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains(DEV_TCP_ENV), "{err}");
        }
    }

    #[test]
    fn grpc_bind_without_the_flag_is_an_error_naming_the_flag() {
        let err = resolve_grpc_listen(None, os("127.0.0.1:50051"), os("/run/user/1000"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("SNITCHWATCH_DEV_TCP=1"), "{err}");
    }

    #[test]
    fn flag_with_the_system_bridge_is_an_error() {
        assert!(resolve_grpc_listen(os("1"), None, None, os("1")).is_err());
        assert!(reject_dev_tcp_in_system_mode(Some(OsStr::new("1"))).is_err());
        assert!(reject_dev_tcp_in_system_mode(None).is_ok());
    }

    #[test]
    fn a_zero_flag_in_system_mode_gives_the_same_error_on_both_paths() {
        let via_resolve = resolve_grpc_listen(os("0"), None, None, os("1"))
            .unwrap_err()
            .to_string();
        let via_reject = reject_dev_tcp_in_system_mode(Some(OsStr::new("0")))
            .unwrap_err()
            .to_string();
        assert_eq!(via_resolve, via_reject);
    }

    #[test]
    fn a_relative_runtime_dir_is_an_error() {
        for dir in ["run/user/1000", ".", "../x"] {
            let err = resolve_grpc_listen(None, None, os(dir), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains("absolute"), "{dir}: {err}");
        }
        assert!(resolve_grpc_listen(None, None, os("/run/user/1000"), None).is_ok());
    }

    #[test]
    fn no_runtime_dir_without_the_flag_is_an_error() {
        assert!(resolve_grpc_listen(None, None, None, None).is_err());
        assert!(resolve_grpc_listen(None, None, os(""), None).is_err());
    }
}
