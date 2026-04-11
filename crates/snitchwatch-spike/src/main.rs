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
//!     cargo run -p snitchwatch-spike -- [bind_addr]
//! Default bind address: 0.0.0.0:50051

use anyhow::{Context, Result};
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:50051".to_string());
    let socket = addr
        .parse()
        .with_context(|| format!("invalid bind address: {addr}"))?;

    info!(%addr, "snitchwatch-spike: starting Ui gRPC server (waiting for opensnitchd to dial in)");

    Server::builder()
        .add_service(UiServer::new(SpikeUi))
        .serve(socket)
        .await
        .context("Ui server crashed")?;

    Ok(())
}
