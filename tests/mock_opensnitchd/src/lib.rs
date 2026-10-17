//! In-process mock of opensnitchd, the gRPC **client** that dials a
//! Snitchwatch bridge.
//!
//! This crate exists for the post-M1.5 topology: the bridge binds the gRPC
//! `Ui` server, and opensnitchd is the client. Tests construct a
//! `MockOpensnitchd::connect(bridge_addr)`, then drive the bridge by calling
//! the same RPCs the real daemon would: `ping`, `subscribe`, `ask_rule`,
//! `notifications`, `post_alert`.

pub mod lists;
pub mod loader;
pub mod round_trip;
mod rule_validation;

use snitchwatch_proto::protocol::ui_client::UiClient;
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, Event, MsgResponse, Notification, NotificationReply,
    NotificationReplyCode, Operator, PingReply, PingRequest, Rule,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};

use rule_validation::validate_duration;
pub use rule_validation::validate_rule_shape;

/// The real daemon's `AskRule` deadline
/// (`vendor/opensnitch/daemon/ui/client.go:366`, `context.WithTimeout(...,
/// time.Second*120)` — confirmed 120s, not the ~15s issue #14 originally
/// guessed). This is the documented real value and stays the default;
/// `ask_rule` applies it so a bridge that never resolves a pending verdict
/// surfaces as a *failure* instead of hanging indefinitely — see issue #14.
pub const ASK_RULE_DEADLINE: Duration = Duration::from_secs(120);

/// Parses and clamps a raw `SNITCHWATCH_MOCK_ASK_RULE_DEADLINE_MS` value
/// against [`ASK_RULE_DEADLINE`] — a pure function so the clamp logic is
/// unit-testable without touching process env state. Issue #14 security
/// review round 2, MEDIUM-3: an earlier version of this override applied the
/// parsed value unconditionally, so
/// `SNITCHWATCH_MOCK_ASK_RULE_DEADLINE_MS=18446744073709551615` (`u64::MAX`)
/// produced a ~584-million-year timeout — the deadline could be effectively
/// disabled, not just shrunk. `.min(ASK_RULE_DEADLINE)` means the override
/// can only ever make the deadline *shorter* than the real daemon value,
/// never longer.
fn clamp_deadline_override(raw: Option<&str>) -> Duration {
    match raw.and_then(|s| s.parse::<u64>().ok()) {
        Some(ms) => Duration::from_millis(ms).min(ASK_RULE_DEADLINE),
        None => ASK_RULE_DEADLINE,
    }
}

/// Test-time override for [`ASK_RULE_DEADLINE`], so a genuine hang fails a
/// test suite in well under a minute instead of 120s. Set
/// `SNITCHWATCH_MOCK_ASK_RULE_DEADLINE_MS` (e.g. via `just test`'s
/// environment) to a millisecond value to shrink it — clamped by
/// [`clamp_deadline_override`] so it can only shrink, never grow past the
/// real 120s. Unset means "use the real 120s value everywhere" — chosen
/// over unconditionally shrinking the constant so this crate still
/// documents (and, without the env var, actually exercises) the real
/// daemon behavior.
///
/// Reads the env var exactly **once** per process, cached in a
/// [`std::sync::OnceLock`] — issue #14 security review round 2, MEDIUM-3:
/// an earlier version called `std::env::var` fresh on every `ask_rule`
/// call, and a test mutated it with `std::env::remove_var` inside a
/// multi-threaded test binary, racing any concurrent `ask_rule` call on
/// another thread. `std::env::set_var`/`remove_var` are unsound to call
/// from multiple threads in current Rust regardless; reading once and
/// caching sidesteps needing to call them at all in the common case where
/// nothing overrides this.
fn ask_rule_deadline() -> Duration {
    static DEADLINE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *DEADLINE.get_or_init(|| {
        let raw = std::env::var("SNITCHWATCH_MOCK_ASK_RULE_DEADLINE_MS").ok();
        clamp_deadline_override(raw.as_deref())
    })
}

/// Errors the mock can surface to tests.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    #[error("connect failed: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("rpc failed: {0}")]
    Rpc(#[from] tonic::Status),
    #[error("ask_rule timed out after {0:?} (real daemon deadline: vendor/opensnitch/daemon/ui/client.go:366)")]
    AskRuleTimedOut(Duration),
    /// Mirrors `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize`: the
    /// real daemon rejects a `Rule` shaped like this outright and falls
    /// through to `DefaultAction` rather than applying the verdict. A bridge
    /// that returns a rule shaped like this passes no differently than one
    /// that hangs — both mean the user's verdict was silently discarded.
    #[error("bridge returned a Rule the daemon would reject: {0}")]
    InvalidRule(String),
}

/// Mock opensnitchd as a gRPC client.
#[derive(Clone)]
pub struct MockOpensnitchd {
    client: UiClient<Channel>,
}

// Same `MockError::Rpc(tonic::Status)` large variant as `validate_rule_shape`
// below. Older clippy exempted these public methods (avoid-breaking-exported-
// api); clippy 1.98 flags them, so allow it here rather than boxing a test
// double's error type.
#[allow(clippy::result_large_err)]
impl MockOpensnitchd {
    /// Wrap a channel the caller already connected (to a bridge's Unix
    /// socket, say, through `Endpoint::connect_with_connector`).
    pub fn from_channel(channel: Channel) -> Self {
        Self {
            client: UiClient::new(channel),
        }
    }

    /// Dial the bridge at `addr`. Caller is responsible for ensuring the
    /// bridge has bound its gRPC port (use `RunningBridge::grpc_addr`).
    pub async fn connect(addr: SocketAddr) -> Result<Self, MockError> {
        let endpoint = Endpoint::from_shared(format!("http://{addr}"))
            .map_err(MockError::Connect)?
            .connect_timeout(Duration::from_secs(2));

        let mut last_err = None;
        for _ in 0..20 {
            match endpoint.connect().await {
                Ok(channel) => {
                    return Ok(Self {
                        client: UiClient::new(channel),
                    });
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        Err(MockError::Connect(last_err.unwrap()))
    }

    pub async fn ping(&mut self, id: u64) -> Result<PingReply, MockError> {
        let reply = self
            .client
            .ping(PingRequest { id, stats: None })
            .await?
            .into_inner();
        Ok(reply)
    }

    /// A `Ping` carrying `Statistics`, as the real daemon sends when it has
    /// new matched events (`rule_misses` and `uptime` feed the bridge's
    /// prompt-slot count).
    pub async fn ping_with_stats(
        &mut self,
        id: u64,
        stats: snitchwatch_proto::protocol::Statistics,
    ) -> Result<PingReply, MockError> {
        let reply = self
            .client
            .ping(PingRequest {
                id,
                stats: Some(stats),
            })
            .await?
            .into_inner();
        Ok(reply)
    }

    /// A `Ping` as the real daemon builds one when it has new events
    /// (`vendor/opensnitch/daemon/statistics/stats.go` `Serialize`): `rules`
    /// is its loader's `NumRules()`, every rule in memory, enabled or not
    /// ([`loader::LoaderModel::num_rules`]), and `uptime` is in seconds.
    pub async fn ping_reporting_rules(
        &mut self,
        id: u64,
        rules: u64,
        uptime: u64,
    ) -> Result<PingReply, MockError> {
        self.ping_with_stats(
            id,
            snitchwatch_proto::protocol::Statistics {
                rules,
                uptime,
                daemon_version: "mock-1.8.0".to_string(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn subscribe(&mut self, name: &str) -> Result<ClientConfig, MockError> {
        let cfg = ClientConfig {
            id: 1,
            name: name.to_string(),
            version: "mock-1.6.0".to_string(),
            ..Default::default()
        };
        let echoed = self.client.subscribe(cfg).await?.into_inner();
        Ok(echoed)
    }

    /// Like [`Self::subscribe`], but takes a full `ClientConfig` the
    /// caller controls — used by tests that need to drive a specific
    /// `is_firewall_running`/`config` value through the bridge.
    pub async fn subscribe_with_config(
        &mut self,
        cfg: ClientConfig,
    ) -> Result<ClientConfig, MockError> {
        let echoed = self.client.subscribe(cfg).await?.into_inner();
        Ok(echoed)
    }

    /// Send a single AskRule unary RPC and wait for the bridge's `Rule` reply.
    ///
    /// Applies the real daemon's ~120s deadline ([`ASK_RULE_DEADLINE`]) so a
    /// bridge that never resolves the pending oneshot fails the test loudly
    /// instead of hanging forever, and validates the returned `Rule` the way
    /// `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` does — see
    /// issue #14, where a bridge that always sent `operator: None` passed
    /// every mock-driven round-trip test while failing 100% of the time
    /// against a real daemon.
    pub async fn ask_rule(&mut self, conn: Connection) -> Result<Rule, MockError> {
        let deadline = ask_rule_deadline();
        let rule = tokio::time::timeout(deadline, self.client.ask_rule(conn))
            .await
            .map_err(|_elapsed| MockError::AskRuleTimedOut(deadline))??
            .into_inner();
        validate_rule_shape(&rule)?;
        Ok(rule)
    }

    /// Like [`Self::ask_rule`], but the deadline is sent as a `grpc-timeout`
    /// header, the way grpc-go's `context.WithTimeout` carries it, instead of
    /// being a local `tokio::time::timeout` the server never hears about.
    /// In tonic 0.12 `Request::set_timeout` only sets that header; tonic's
    /// `GrpcTimeout` layer enforces it on both the client channel and the
    /// server, so when it expires the bridge's handler future is dropped,
    /// whichever side's timer fires first. An expired deadline surfaces as
    /// `MockError::Rpc` with `Cancelled` or `DeadlineExceeded`.
    pub async fn ask_rule_with_deadline(
        &mut self,
        conn: Connection,
        deadline: Duration,
    ) -> Result<Rule, MockError> {
        let mut request = tonic::Request::new(conn);
        request.set_timeout(deadline);
        let rule = self.client.ask_rule(request).await?.into_inner();
        validate_rule_shape(&rule)?;
        Ok(rule)
    }

    pub async fn post_alert(&mut self, alert: Alert) -> Result<MsgResponse, MockError> {
        let reply = self.client.post_alert(alert).await?.into_inner();
        Ok(reply)
    }

    /// Convenience wrapper around [`Self::post_alert`] for the common case
    /// of a text-payload ERROR/WARNING alert (mirrors
    /// `vendor/opensnitch/daemon/ui/alerts.go`'s `NewErrorAlert`/
    /// `NewWarningAlert` shape) — used by tests exercising the daemon-alert
    /// → diagnostics overlay (issue #6) without hand-building an `Alert`.
    pub async fn post_alert_text(
        &mut self,
        r#type: snitchwatch_proto::protocol::alert::Type,
        what: snitchwatch_proto::protocol::alert::What,
        text: &str,
    ) -> Result<MsgResponse, MockError> {
        self.post_alert(Alert {
            id: 1,
            r#type: r#type as i32,
            action: 0,
            priority: 0,
            what: what as i32,
            data: Some(snitchwatch_proto::protocol::alert::Data::Text(
                text.to_string(),
            )),
        })
        .await
    }

    /// Open the bidi `Notifications` stream. Like the real daemon
    /// (`vendor/opensnitch/daemon/ui/notifications.go` `listenForNotifications`),
    /// the first message it sends is a HELLO: `NotificationReply { id: 0, OK }`.
    /// The bridge handles it asynchronously; wait on
    /// `RunningBridge::daemon_stream_ready` before relying on it.
    ///
    /// Yields each inbound [`Notification`] whole. It previously forwarded only
    /// `n.id` and discarded the rest, which made it impossible to assert what
    /// the bridge actually commanded — a test could only count notifications,
    /// not check that a rule toggle carried the right action or a rule the
    /// daemon would accept.
    ///
    /// Deliberately does NOT reject invalid rules itself: the real daemon does
    /// not close the stream over a bad rule, it fails that one rule and replies
    /// `NotificationReplyCode_ERROR`. Tests assert daemon-acceptance explicitly
    /// with [`validate_rule_shape`], which mirrors the daemon's real acceptance
    /// path (`Deserialize` *and* `Operator.Compile()`).
    pub async fn open_notifications(
        &mut self,
    ) -> Result<
        (
            mpsc::Sender<NotificationReply>,
            mpsc::Receiver<Notification>,
        ),
        MockError,
    > {
        let (reply_tx, reply_rx) = mpsc::channel::<NotificationReply>(16);
        reply_tx
            .try_send(NotificationReply {
                id: 0,
                code: NotificationReplyCode::Ok as i32,
                data: String::new(),
            })
            .expect("a fresh channel has room for the HELLO");
        let outbound = tokio_stream::wrappers::ReceiverStream::new(reply_rx);

        let mut inbound = self.client.notifications(outbound).await?.into_inner();

        let (ntf_tx, ntf_rx) = mpsc::channel::<Notification>(16);
        tokio::spawn(async move {
            while let Ok(Some(n)) = inbound.message().await {
                if ntf_tx.send(n).await.is_err() {
                    return;
                }
            }
        });

        Ok((reply_tx, ntf_rx))
    }
}

/// The description the bazzite-tower opensnitchd fork gives the synthetic
/// rule of a connection its `DefaultAction` decided (E3, plan
/// `docs/superpowers/plans/2026-10-08-default-applied-events.md`). Kept here
/// as the fork's side of the contract; a test checks it against the bridge's.
pub const DEFAULT_ACTION_MARKER: &str = "snitchwatch:default-action";

/// The `Statistics.events[]` entry the fork appends when no rule matched
/// `conn` and its `DefaultAction` decided it (an Ask failed or timed out, or
/// the prompt slot was busy). `action` is the action it applied: `allow`,
/// `deny` or `reject`. Stock v1.8.0 appends nothing for such a connection.
///
/// Each one grows the daemon's `rule_misses`, never `rule_hits`, so a ping
/// carrying it must count it there. `time` is left empty: the bridge reads
/// `unixnano` only.
pub fn default_action_event(conn: Connection, action: &str, unixnano: i64) -> Event {
    Event {
        time: String::new(),
        connection: Some(conn),
        rule: Some(Rule {
            created: unixnano / 1_000_000_000,
            name: String::new(),
            description: DEFAULT_ACTION_MARKER.to_string(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: action.to_string(),
            duration: "once".to_string(),
            operator: Some(Operator {
                r#type: "simple".to_string(),
                operand: "true".to_string(),
                data: String::new(),
                sensitive: false,
                list: vec![],
            }),
        }),
        unixnano,
    }
}

#[cfg(test)]
mod tests;
