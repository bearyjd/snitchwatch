//! M0 spike — verify that opensnitchd's `Ui` gRPC service supports the
//! "ask on new connection" UX we want.
//!
//! ## Architectural correction (vs. plan)
//!
//! The plan assumed the bridge would dial opensnitchd as a gRPC *client*.
//! Reading `vendor/opensnitch/proto/ui.proto` lines 263–272 disproves this:
//!
//! > Notification message is sent to the clients (daemons) from the GUI
//! > (server) for several purposes...
//!
//! opensnitchd is the gRPC **client**. The GUI is the gRPC **server**. The
//! daemon's `Server.Address` config tells it where to dial. So the spike is a
//! tonic server that implements the `Ui` trait, binds a TCP port, prints
//! every `AskRule` it receives, reads y/n from stdin, and replies with an
//! allow/deny `Rule`.
//!
//! Usage:
//!     cargo run -p snitchwatch-spike
//!     SNITCHWATCH_DEV_TCP=1 cargo run -p snitchwatch-spike -- [loopback_addr]
//!
//! Default: a Unix socket at `$XDG_RUNTIME_DIR/snitchwatch-grpc/spike.sock`
//! (#35). TCP needs `SNITCHWATCH_DEV_TCP=1` and a loopback address
//! (default `127.0.0.1:50051`); anything else, including `0.0.0.0`, is refused.

use anyhow::{Context, Result};
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio_stream::Stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

/// Spike implementation of the `Ui` service.
///
/// All methods are deliberately minimal — the goal is to confirm the daemon
/// will dial in, send `AskRule` for new connections, and accept our verdicts.
#[derive(Default)]
struct SpikeUi;

#[tonic::async_trait]
impl Ui for SpikeUi {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let req = request.into_inner();
        info!(id = req.id, "ping received");
        Ok(Response::new(PingReply { id: req.id }))
    }

    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        let conn = request.into_inner();
        println!(
            "\nAskRule: pid={} {} {}://{}:{} -> {}:{} ({})",
            conn.process_id,
            conn.process_path,
            conn.protocol,
            conn.src_ip,
            conn.src_port,
            if conn.dst_host.is_empty() {
                conn.dst_ip.clone()
            } else {
                conn.dst_host.clone()
            },
            conn.dst_port,
            conn.process_path,
        );

        let action = read_verdict_from_stdin().await;
        info!(action = %action, "verdict");

        Ok(Response::new(Rule {
            created: now_unix_secs() as i64,
            // Use a non-empty name so the daemon doesn't reject it.
            name: format!("spike-{}", action),
            description: "M0 spike interactive verdict".to_string(),
            enabled: true,
            precedence: false,
            nolog: false,
            action,
            // Empty duration string means "once" in opensnitchd terms.
            duration: "once".to_string(),
            operator: None,
        }))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        let cfg = request.into_inner();
        info!(
            client = %cfg.name,
            version = %cfg.version,
            rules = cfg.rules.len(),
            "client subscribed"
        );
        // Echo the config back unchanged — daemon expects a ClientConfig
        // response acknowledging the subscription.
        Ok(Response::new(cfg))
    }

    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send + 'static>>;

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        info!("notifications stream opened");
        let mut inbound = request.into_inner();

        // Spawn a task that drains the inbound NotificationReply stream and
        // logs each reply. The spike does not push any Notifications back to
        // the daemon — we just need a live stream so the daemon stays happy.
        tokio::spawn(async move {
            while let Ok(Some(reply)) = inbound.message().await {
                info!(
                    id = reply.id,
                    code = reply.code,
                    data = %reply.data,
                    "notification reply from daemon"
                );
            }
            warn!("notification reply stream ended");
        });

        let outbound = async_stream::try_stream! {
            // Yield nothing — keep the stream open until the daemon hangs up.
            // `futures::future::pending` would also work but pulling it in
            // just for this is overkill.
            let () = std::future::pending().await;
            yield Notification::default();
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::NotificationsStream
        ))
    }

    async fn post_alert(&self, request: Request<Alert>) -> Result<Response<MsgResponse>, Status> {
        let alert = request.into_inner();
        info!(id = alert.id, type_ = alert.r#type, "alert received");
        Ok(Response::new(MsgResponse { id: alert.id }))
    }
}

/// Read a single y/N line from stdin and translate to opensnitchd's
/// rule action string ("allow" or "deny").
async fn read_verdict_from_stdin() -> String {
    use tokio::io::{AsyncBufReadExt, BufReader};
    print_prompt();
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return "deny".to_string();
    }
    if line.trim().eq_ignore_ascii_case("y") {
        "allow".to_string()
    } else {
        "deny".to_string()
    }
}

fn print_prompt() {
    use std::io::Write;
    print!("allow? [y/N] ");
    let _ = std::io::stdout().flush();
}

/// Current unix timestamp in seconds. Avoids pulling in chrono for the spike.
fn now_unix_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const DEV_TCP_ENV: &str = "SNITCHWATCH_DEV_TCP";
const DEFAULT_DEV_ADDR: &str = "127.0.0.1:50051";

/// Where the spike listens.
#[derive(Debug, PartialEq, Eq)]
enum Listen {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

/// Pure over its inputs so it is unit-testable without touching the env.
fn resolve_listen(
    dev_flag: Option<OsString>,
    addr_arg: Option<String>,
    xdg_runtime_dir: Option<OsString>,
) -> Result<Listen> {
    match dev_flag.as_deref().map(|v| v.to_str()) {
        None => {}
        Some(Some("1")) => {
            let raw = addr_arg.unwrap_or_else(|| DEFAULT_DEV_ADDR.to_string());
            let addr: SocketAddr = raw
                .parse()
                .with_context(|| format!("invalid bind address: {raw}"))?;
            anyhow::ensure!(
                addr.ip().is_loopback(),
                "bind address must be loopback (got {addr}); {DEV_TCP_ENV} never exposes gRPC beyond loopback"
            );
            return Ok(Listen::Tcp(addr));
        }
        Some(_) => anyhow::bail!("{DEV_TCP_ENV} must be 1 when set"),
    }
    if let Some(arg) = addr_arg {
        anyhow::bail!("TCP address {arg} is dev-only: set {DEV_TCP_ENV}=1 to use it");
    }
    let xdg = xdg_runtime_dir
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .with_context(|| {
            format!("XDG_RUNTIME_DIR must be an absolute path (or set {DEV_TCP_ENV}=1 for loopback TCP)")
        })?;
    Ok(Listen::Unix(
        xdg.join("snitchwatch-grpc").join("spike.sock"),
    ))
}

// Small local copy of snitchwatch-bridge-cli's `user_socket` helpers (which
// are crate-private): depending on the bridge crate just for these would add
// a heavy edge to a throwaway spike.
fn bind_unix(path: &Path) -> Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    bind_unix_with(path, |p| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
    })
}

/// `chmod_socket` is the final 0600 step, injectable so a failure is testable.
fn bind_unix_with(
    path: &Path,
    chmod_socket: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
    let dir = path.parent().context("socket path has no parent")?;
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).context("cannot create socket directory"),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    let euid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        meta.file_type().is_dir() && meta.uid() == euid && meta.mode() & 0o7777 == 0o700,
        "{} must be a real directory owned by uid {euid} with mode 0700",
        dir.display()
    );
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => anyhow::bail!("{} exists and is not a socket", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot stat socket path"),
    }
    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("cannot bind {}", path.display()))?;
    if let Err(e) = chmod_socket(path) {
        // Never leave a socket with an unintended mode behind.
        let _ = std::fs::remove_file(path);
        return Err(e).with_context(|| format!("cannot chmod socket {}", path.display()));
    }
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let listen = resolve_listen(
        std::env::var_os(DEV_TCP_ENV),
        std::env::args().nth(1),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )?;

    info!(
        ?listen,
        "snitchwatch-spike: starting Ui gRPC server (waiting for opensnitchd to dial in)"
    );

    let server = Server::builder().add_service(UiServer::new(SpikeUi));
    match listen {
        Listen::Tcp(addr) => server.serve(addr).await,
        Listen::Unix(path) => {
            let listener = tokio::net::UnixListener::from_std(bind_unix(&path)?)?;
            server
                .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener))
                .await
        }
    }
    .context("Ui server crashed")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn a_failed_final_chmod_removes_the_socket_and_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snitchwatch-grpc").join("spike.sock");
        let err = bind_unix_with(&path, |_| Err(std::io::Error::other("boom"))).unwrap_err();
        assert!(err.to_string().contains("chmod"), "{err}");
        assert!(!path.exists(), "socket must not be left behind");
    }

    #[test]
    fn bind_unix_yields_a_0600_socket() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snitchwatch-grpc").join("spike.sock");
        let _l = bind_unix(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    }

    #[test]
    fn default_is_the_spike_socket_under_snitchwatch_grpc() {
        let got = resolve_listen(None, None, os("/run/user/1000")).unwrap();
        assert_eq!(
            got,
            Listen::Unix("/run/user/1000/snitchwatch-grpc/spike.sock".into())
        );
    }

    #[test]
    fn default_needs_an_absolute_runtime_dir() {
        assert!(resolve_listen(None, None, None).is_err());
        assert!(resolve_listen(None, None, os("rel")).is_err());
    }

    #[test]
    fn flag_gives_loopback_tcp_default() {
        let got = resolve_listen(os("1"), None, None).unwrap();
        assert_eq!(got, Listen::Tcp("127.0.0.1:50051".parse().unwrap()));
    }

    #[test]
    fn flag_accepts_an_explicit_loopback_address() {
        let got = resolve_listen(os("1"), Some("127.0.0.1:0".into()), None).unwrap();
        assert_eq!(got, Listen::Tcp("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn flag_refuses_0_0_0_0_and_garbage() {
        assert!(resolve_listen(os("1"), Some("0.0.0.0:50051".into()), None).is_err());
        assert!(resolve_listen(os("1"), Some("nonsense".into()), None).is_err());
    }

    #[test]
    fn address_without_the_flag_is_refused_naming_it() {
        let err =
            resolve_listen(None, Some("127.0.0.1:50051".into()), os("/run/user/1000")).unwrap_err();
        assert!(err.to_string().contains("SNITCHWATCH_DEV_TCP=1"), "{err}");
    }

    #[test]
    fn non_1_flag_is_refused() {
        assert!(resolve_listen(os("0"), None, os("/run/user/1000")).is_err());
    }
}
