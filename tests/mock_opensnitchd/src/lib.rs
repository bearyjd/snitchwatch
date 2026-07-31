//! In-process mock of opensnitchd, the gRPC **client** that dials a
//! Snitchwatch bridge.
//!
//! This crate exists for the post-M1.5 topology: the bridge binds the gRPC
//! `Ui` server, and opensnitchd is the client. Tests construct a
//! `MockOpensnitchd::connect(bridge_addr)`, then drive the bridge by calling
//! the same RPCs the real daemon would: `ping`, `subscribe`, `ask_rule`,
//! `notifications`, `post_alert`.

use snitchwatch_proto::protocol::ui_client::UiClient;
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, NotificationReply, PingReply, PingRequest, Rule,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};

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

impl MockOpensnitchd {
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

    /// Open the bidi `Notifications` stream.
    pub async fn open_notifications(
        &mut self,
    ) -> Result<(mpsc::Sender<NotificationReply>, mpsc::Receiver<u64>), MockError> {
        let (reply_tx, reply_rx) = mpsc::channel::<NotificationReply>(16);
        let outbound = tokio_stream::wrappers::ReceiverStream::new(reply_rx);

        let mut inbound = self.client.notifications(outbound).await?.into_inner();

        let (count_tx, count_rx) = mpsc::channel::<u64>(16);
        tokio::spawn(async move {
            while let Ok(Some(n)) = inbound.message().await {
                if count_tx.send(n.id).await.is_err() {
                    return;
                }
            }
        });

        Ok((reply_tx, count_rx))
    }
}

/// Reject the class of malformed `Rule` a real daemon rejects — mirroring
/// its ACTUAL acceptance path, not just `rule.Deserialize`. Two prior
/// versions of this function only checked `Deserialize`
/// (`vendor/opensnitch/daemon/rule/rule.go:85-89`, `operator: None` only),
/// but real rejection mostly happens later, in `Operator.Compile()`
/// (`vendor/opensnitch/daemon/rule/operator.go:109-214`, called from
/// `loader.go:408` when the daemon loads/applies the rule) — `Deserialize`'s
/// own `NewOperator` call never errors. See issue #14 security review FIX 4.
///
/// Checks, in the order the daemon would effectively hit them:
///   1. `operator: None` (`Deserialize`, rule.go:85-89).
///   2. Unknown `operator.type` (`Compile`, operator.go:207 — the final
///      `else` branch, "Unknown Operator type").
///   3. A `regexp`-typed operator whose `data` doesn't compile
///      (`Compile`, operator.go:146-149) — lowercased first when
///      `sensitive == false`, exactly as the daemon does, since case
///      matters for what actually gets handed to `regexp.Compile`.
///   4. `rule.duration` that's neither a named duration (`once`,
///      `until restart`, `always`) nor `time.ParseDuration`-parseable
///      (`loader.go:326` `isTemporary`, `loader.go:441`
///      `scheduleTemporaryRule`'s `time.ParseDuration` call).
///   5. An unsafe/empty rule name (`loader.go:162`) — this is a
///      bridge-side contract stricter than what the daemon itself enforces
///      (it has no such check at all, which is exactly issue #14 security
///      review FIX 1's finding), kept here as a regression canary: this
///      check is what would have caught FIX 1 at test time.
// `MockError::Rpc(tonic::Status)` is already the large variant driving this;
// the crate's public `MockError`-returning methods (`ask_rule`, `ping`, ...)
// are exempted from this lint as public API, so this private helper needs
// the same allowance rather than boxing just for its own sake.
#[allow(clippy::result_large_err)]
fn validate_rule_shape(rule: &Rule) -> Result<(), MockError> {
    validate_rule_name(&rule.name)?;
    validate_duration(&rule.duration)?;

    let operator = rule
        .operator
        .as_ref()
        .ok_or_else(|| MockError::InvalidRule("operator is None".to_string()))?;
    validate_operator_compiles(operator)?;
    Ok(())
}

/// The `operator.type` vocabulary the daemon actually recognizes
/// (`vendor/opensnitch/daemon/rule/operator.go`'s `Type` consts: `Simple`,
/// `Regexp`, `Complex`, `List`, `Network`, `Lists`).
const KNOWN_OPERATOR_TYPES: &[&str] = &["simple", "regexp", "complex", "list", "network", "lists"];

/// Mirrors `Operator.Compile()` (operator.go:109-214) for the subset this
/// bridge actually emits (`simple`/`regexp` — see `translator::verdict`).
/// Not a full port: `network`/`lists` compilation depends on daemon-local
/// state (alias cache, loaded blocklists) this mock has no access to, and
/// this bridge never emits those types for an `AskRule` reply, so they're
/// only checked for a known type, not fully compiled.
/// The `operator.operand` vocabulary the daemon recognizes
/// (`vendor/opensnitch/daemon/rule/operator.go`'s `Operand` consts,
/// lines 31-56). `process.env.` is a *prefix* (`OpProcessEnvPrefix`), not an
/// exact value — `process.env.PATH` is a valid operand — so it's checked
/// separately in [`is_known_operand`], not listed here verbatim.
const KNOWN_OPERANDS: &[&str] = &[
    "true",
    "process.id",
    "process.path",
    "process.parent.path",
    "process.command",
    "process.hash.md5",
    "process.hash.sha1",
    "user.id",
    "user.name",
    "source.ip",
    "source.port",
    "dest.ip",
    "dest.host",
    "dest.port",
    "dest.network",
    "source.network",
    "protocol",
    "iface.in",
    "iface.out",
    "list",
    "lists.domains",
    "lists.domains_regexp",
    "lists.ips",
    "lists.nets",
    "lists.hash.md5",
];

fn is_known_operand(operand: &str) -> bool {
    KNOWN_OPERANDS.contains(&operand) || operand.starts_with("process.env.")
}

/// Issue #14 security review round 2, LOW: `Operator.Compile()`
/// (`operator.go:109-214`) doesn't actually validate `Operand` against this
/// vocabulary for every `Type` — for `Simple`, `Compile` sets its callback
/// unconditionally regardless of what `Operand` says. An unknown/typo'd
/// operand still "compiles" successfully; the daemon only discovers the
/// problem when a *later*, separate operand-to-connection-field dispatch
/// (outside `operator.go`) falls through to nothing for the value, so the
/// rule silently never matches and every connection through it falls
/// through to `DefaultAction` — structurally the same false-pass class as
/// issue #14 itself (a `Rule` that "looks" accepted but never actually
/// applies). This check is stricter than `Compile()` on purpose, as a test-
/// time canary for that failure mode, mirroring [`validate_rule_name`]'s
/// FIX-1-canary role.
#[allow(clippy::result_large_err)]
fn validate_operator_compiles(op: &snitchwatch_proto::protocol::Operator) -> Result<(), MockError> {
    if !KNOWN_OPERATOR_TYPES.contains(&op.r#type.as_str()) {
        return Err(MockError::InvalidRule(format!(
            "unknown operator type: `{}`",
            op.r#type
        )));
    }
    if !is_known_operand(&op.operand) {
        return Err(MockError::InvalidRule(format!(
            "unknown operator operand: `{}`",
            op.operand
        )));
    }

    if op.r#type == "regexp" {
        // operator.go:146-148: lowercased before compiling when
        // Sensitive == false — case affects what actually gets compiled.
        let data = if op.sensitive {
            op.data.clone()
        } else {
            op.data.to_lowercase()
        };
        if let Err(e) = regex::Regex::new(&data) {
            return Err(MockError::InvalidRule(format!(
                "operator.data does not compile as a regexp: {e}"
            )));
        }
    }

    Ok(())
}

/// The daemon's three named `Rule.duration` values
/// (`vendor/opensnitch/daemon/rule/rule.go:31-34`).
const NAMED_DURATIONS: &[&str] = &["once", "until restart", "always"];

#[allow(clippy::result_large_err)]
fn validate_duration(duration: &str) -> Result<(), MockError> {
    if NAMED_DURATIONS.contains(&duration) {
        return Ok(());
    }
    if looks_like_go_duration(duration) {
        return Ok(());
    }
    Err(MockError::InvalidRule(format!(
        "duration `{duration}` is neither a named duration ({NAMED_DURATIONS:?}) nor \
         Go-duration-parseable"
    )))
}

/// Approximates Go's `time.ParseDuration` grammar closely enough to catch
/// the malformed-duration class of bug: optional sign, then one or more
/// `<number><unit>` segments (`ns`/`us`/`µs`/`ms`/`s`/`m`/`h`), or the bare
/// literal `0` (which Go's parser special-cases as valid with no unit). This
/// is NOT a byte-for-byte port of Go's parser — it doesn't need to be, only
/// to reject/accept the same shapes this bridge could plausibly produce or
/// regress into.
fn looks_like_go_duration(s: &str) -> bool {
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if unsigned.is_empty() {
        return false;
    }
    if unsigned == "0" {
        return true;
    }
    static GO_DURATION_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = GO_DURATION_RE.get_or_init(|| {
        regex::Regex::new(r"^([0-9]+(\.[0-9]*)?(ns|us|µs|ms|s|m|h))+$").expect("valid regex")
    });
    re.is_match(unsigned)
}

/// Regression canary for issue #14 security review FIX 1: the daemon has NO
/// rule-name validation of its own (it writes `Rule.name` verbatim to
/// `<rules-dir>/<name>.json`, root-owned — see `validate_rule_shape`'s doc),
/// so this bridge-side check is stricter than the real acceptance path on
/// purpose. It exists to make a regression in
/// `translator::verdict::sanitize_host_for_rule_name` fail a test instead of
/// silently reintroducing a path-traversal-via-rule-name bug.
#[allow(clippy::result_large_err)]
fn validate_rule_name(name: &str) -> Result<(), MockError> {
    if name.is_empty() {
        return Err(MockError::InvalidRule("rule name is empty".to_string()));
    }
    if name.contains('/') {
        return Err(MockError::InvalidRule(format!(
            "rule name contains a path separator: `{name}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_bridge::cache::connections::ConnectionCache;
    use snitchwatch_bridge::grpc_server::UiService;
    use snitchwatch_bridge::ws_messages::ServerMessage;
    use std::sync::Arc;
    use tokio::sync::{broadcast, Mutex};
    use tonic::transport::Server;

    async fn spawn_bridge_grpc() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
        let tray_pub = Arc::new(snitchwatch_bridge::tray_state::TrayStatePublisher::new());
        let notice_bus = Arc::new(snitchwatch_bridge::notice::NoticeBus::new());
        let filtering_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr
    }

    #[tokio::test]
    async fn mock_can_ping_bridge() {
        let addr = spawn_bridge_grpc().await;
        let mut mock = MockOpensnitchd::connect(addr).await.unwrap();
        let reply = mock.ping(123).await.unwrap();
        assert_eq!(reply.id, 123);
    }

    #[tokio::test]
    async fn mock_can_subscribe_to_bridge() {
        let addr = spawn_bridge_grpc().await;
        let mut mock = MockOpensnitchd::connect(addr).await.unwrap();
        let echoed = mock.subscribe("opensnitchd-mock").await.unwrap();
        assert_eq!(echoed.name, "opensnitchd-mock");
    }

    // -- validate_rule_shape / FIX 4 (issue #14 security review) ---------

    fn valid_operator() -> snitchwatch_proto::protocol::Operator {
        snitchwatch_proto::protocol::Operator {
            r#type: "simple".to_string(),
            operand: "dest.host".to_string(),
            data: "github.com".to_string(),
            sensitive: false,
            list: Vec::new(),
        }
    }

    fn valid_rule() -> Rule {
        Rule {
            created: 0,
            name: "snitchwatch-allow-github.com-443".to_string(),
            description: String::new(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: "allow".to_string(),
            duration: "once".to_string(),
            operator: Some(valid_operator()),
        }
    }

    #[test]
    fn validate_rule_shape_accepts_a_well_formed_rule() {
        assert!(validate_rule_shape(&valid_rule()).is_ok());
    }

    #[test]
    fn validate_rule_shape_rejects_none_operator() {
        let mut rule = valid_rule();
        rule.operator = None;
        assert!(validate_rule_shape(&rule).is_err());
    }

    #[test]
    fn validate_rule_shape_rejects_unknown_operator_type() {
        let mut rule = valid_rule();
        rule.operator.as_mut().unwrap().r#type = "bogus".to_string();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(
            matches!(err, MockError::InvalidRule(msg) if msg.contains("unknown operator type"))
        );
    }

    #[test]
    fn validate_rule_shape_rejects_uncompilable_regexp_data() {
        let mut rule = valid_rule();
        let op = rule.operator.as_mut().unwrap();
        op.r#type = "regexp".to_string();
        op.data = "(unclosed".to_string();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("does not compile")));
    }

    #[test]
    fn validate_rule_shape_accepts_a_compilable_regexp() {
        let mut rule = valid_rule();
        let op = rule.operator.as_mut().unwrap();
        op.r#type = "regexp".to_string();
        op.data = r"^(?:[^.]+\.)*example\.com$".to_string();
        assert!(validate_rule_shape(&rule).is_ok());
    }

    #[test]
    fn validate_rule_shape_rejects_unnamed_unparseable_duration() {
        let mut rule = valid_rule();
        rule.duration = "not-a-duration".to_string();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("duration")));
    }

    #[test]
    fn validate_rule_shape_accepts_go_duration_strings() {
        for duration in ["5m", "30s", "1h30m", "0", "until restart", "always", "once"] {
            let mut rule = valid_rule();
            rule.duration = duration.to_string();
            assert!(
                validate_rule_shape(&rule).is_ok(),
                "expected `{duration}` to be accepted"
            );
        }
    }

    #[test]
    fn validate_rule_shape_rejects_empty_rule_name() {
        let mut rule = valid_rule();
        rule.name = String::new();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("empty")));
    }

    #[test]
    fn validate_rule_shape_rejects_rule_name_with_path_separator() {
        // This is the check that would have caught issue #14 FIX 1 (path
        // traversal via an unsanitized rule name) at test time.
        let mut rule = valid_rule();
        rule.name = "../../../../etc/cron.d/x".to_string();
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(matches!(err, MockError::InvalidRule(msg) if msg.contains("path separator")));
    }

    #[test]
    fn ask_rule_deadline_returns_the_real_value_or_less() {
        // Doesn't touch env state (see `ask_rule_deadline`'s doc comment on
        // why: `OnceLock`-cached, and `set_var`/`remove_var` are unsound to
        // call from a multi-threaded test binary). Whatever this process's
        // env happened to have at first call, the deadline can never exceed
        // the real value.
        assert!(ask_rule_deadline() <= ASK_RULE_DEADLINE);
    }

    // -- clamp_deadline_override / FIX MEDIUM-3 (issue #14 security review) --

    #[test]
    fn deadline_override_defaults_to_real_value_when_unset() {
        assert_eq!(clamp_deadline_override(None), ASK_RULE_DEADLINE);
    }

    #[test]
    fn deadline_override_shrinks_when_smaller() {
        assert_eq!(
            clamp_deadline_override(Some("500")),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn deadline_override_cannot_exceed_the_real_deadline() {
        // u64::MAX ms is ~584 million years — must clamp down to the real
        // 120s value, not disable the deadline.
        assert_eq!(
            clamp_deadline_override(Some(&u64::MAX.to_string())),
            ASK_RULE_DEADLINE
        );
    }

    #[test]
    fn deadline_override_ignores_unparseable_values() {
        assert_eq!(
            clamp_deadline_override(Some("not-a-number")),
            ASK_RULE_DEADLINE
        );
    }

    // -- is_known_operand / LOW (issue #14 security review) ---------------

    #[test]
    fn validate_rule_shape_rejects_unknown_operand() {
        let mut rule = valid_rule();
        rule.operator.as_mut().unwrap().operand = "dest.hostt".to_string(); // typo
        let err = validate_rule_shape(&rule).unwrap_err();
        assert!(
            matches!(err, MockError::InvalidRule(msg) if msg.contains("unknown operator operand"))
        );
    }

    #[test]
    fn validate_rule_shape_accepts_process_env_prefixed_operand() {
        let mut rule = valid_rule();
        rule.operator.as_mut().unwrap().operand = "process.env.PATH".to_string();
        assert!(validate_rule_shape(&rule).is_ok());
    }
}
