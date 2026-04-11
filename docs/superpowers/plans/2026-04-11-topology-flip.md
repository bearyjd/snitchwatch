# Snitchwatch M1.5 — Topology Flip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Invert the bridge's gRPC topology so the Snitchwatch GUI binds and serves the OpenSnitch `protocol.UI` service while opensnitchd connects to it as a gRPC client — matching the real protocol discovered during the M0 spike.

**Architecture:** The bridge keeps its WebSocket server (axum on `127.0.0.1:0`) for the UI. Alongside it, a new tonic gRPC server binds another local port and implements all five RPCs of `protocol.UI`. `AskRule` becomes a unary RPC handler that materializes a pending `ConnectionRow` from the incoming `Connection`, broadcasts it on the WS, awaits the existing `oneshot<Verdict>` cache machinery, then synthesises a `Rule` reply. The JSON envelope hack inside `Notification.data` and the entire `grpc_client.rs` reconnect helper are deleted. The mock is rewritten as a tonic **client** that dials the bridge.

**Tech Stack:** tonic 0.12 (server side), prost 0.13, tokio 1.40, axum 0.7 (unchanged for WS), tracing 0.1, anyhow 1, async-stream 0.3 (already a workspace dep) for the bidi `Notifications` stream, async_trait via `#[tonic::async_trait]`.

**What this plan does NOT cover:**
- Vendoring or rebranding the LS-for-Linux web UI (Plan 3 — M2).
- The Tauri shell (Plan 4 — M3).
- Live `opensnitchd` smoke test in a rootful podman quadlet (Plan 7 — M6).
- `cargo-llvm-cov` ≥80% coverage on translator/cache (Plan 7 — M6).
- Multiple WS bind modes; the WS bind stays ephemeral on `127.0.0.1:0` until Plan 3.

---

## Memory Constraints (read before starting)

These guard rails come from `~/.claude/projects/-var-home-user-Documents-vibe-code-opensnitch-gui/memory/`:

1. **`clippy_gotchas_bridge.md`** — anywhere a `oneshot::Receiver<Verdict>` is dropped, use `drop(...)`, never `let _ = ...`. If a translated value carries a `ConnectionRow`, keep it boxed (`Box<ConnectionRow>`) to avoid `clippy::large_enum_variant`.
2. **`m1_envelope_hack.md`** — the JSON envelope inside `Notification.data` is M1 scaffolding. Delete its consumers; do NOT promote envelope fields into the M1.5/M2 contract.
3. **`bash_antipattern_hook.md`** — workspace blocks `find`/`ls`/`cat`/`grep`/`rg`/`head`/`tail`/`sed`/`awk` in Bash. Use Read/Grep/Glob.
4. **`autonomous_tdd_resume.md`** — on resume after compaction, advance the next task with a tool call; don't recap.
5. **`plan1_deferred_criteria.md`** — Plan 1 is complete at commit `a54c0b4`. The two deferred items (live opensnitchd, llvm-cov) are environmental and belong to Plan 7.

---

## File Structure

### NEW files
- `crates/snitchwatch-bridge/src/grpc_server.rs` — `UiService` struct + `#[tonic::async_trait] impl Ui for UiService` covering all 5 RPCs.
- `crates/snitchwatch-bridge/src/translator/connection.rs` — pure `connection_to_row(conn: &Connection, notification_id: u64) -> ConnectionRow`.
- `crates/snitchwatch-bridge/src/translator/verdict.rs` — pure `verdict_to_rule(verdict: Verdict, conn: &Connection, now_secs: i64) -> Rule`.

### MODIFIED files
- `crates/snitchwatch-bridge/src/lib.rs` — re-export `grpc_server` instead of `grpc_client`; module list updated.
- `crates/snitchwatch-bridge/src/translator/mod.rs` — drop `pub mod downstream;`, add `pub mod connection;` and `pub mod verdict;`.
- `crates/snitchwatch-bridge/src/ws_messages.rs` — drop the now-stale `TODO(task-7-followup)` note about envelope payload capture.
- `crates/snitchwatch-bridge-cli/src/lib.rs` — replace the gRPC dial-and-pump flow with `Server::builder().add_service(UiServer::new(UiService::new(...))).serve_with_shutdown(...)`. Drop `downstream::translate_notification`, the downstream pump, and `verdict_to_json`. Add `grpc_bind` to `BridgeConfig` and `grpc_addr` to `RunningBridge`.
- `crates/snitchwatch-bridge-cli/src/main.rs` — print `GRPC_LISTEN_ADDR=...` next to `WS_LISTEN_ADDR=...`; read `SNITCHWATCH_GRPC_BIND` env var.
- `tests/mock_opensnitchd/src/lib.rs` — rewritten as a tonic **client** that dials the bridge, sends `AskRule(Connection)` synchronously, opens the `Notifications` bidi stream, and exposes scripted `Connection` payloads + collected `Rule` responses.
- `tests/bridge_protocol_test.rs` — replaces the envelope-via-Notification round-trip with a unary `AskRule` round-trip.
- `crates/snitchwatch-bridge/Cargo.toml` — add `async-stream` as a dependency.
- `docs/m0-spike-findings.md` — append "Adjustment applied" footer with the resulting commit hash blanks (filled in at task 11).
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` — flip the milestone table to mark M1 done and add an M1.5 row.
- `README.md` — replace the "Bridge dials opensnitchd" text with the inverted-topology description.

### DELETED files
- `crates/snitchwatch-bridge/src/grpc_client.rs` — replaced by `grpc_server.rs`.
- `crates/snitchwatch-bridge/src/translator/downstream.rs` — its `Translated::AskRule(Box<ConnectionRow>)` flow becomes the body of the new `ask_rule` server handler; the `Translated::Ignored` branch is no longer reachable since AskRule is its own unary RPC.

---

## Part A — Pure translators (no I/O)

### Task 1: `connection_to_row` — Connection proto → ConnectionRow

**Files:**
- Create: `crates/snitchwatch-bridge/src/translator/connection.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/mod.rs`

- [ ] **Step 1: Add the module declaration**

Edit `crates/snitchwatch-bridge/src/translator/mod.rs`. Add the new module line alongside the existing ones:

```rust
pub mod connection;
```

- [ ] **Step 2: Write the failing test file**

Create `crates/snitchwatch-bridge/src/translator/connection.rs` with the test only (no implementation yet):

```rust
//! Translate an opensnitchd `Connection` proto into a `ConnectionRow` for
//! the WebSocket layer.
//!
//! The `notification_id` argument is the daemon-supplied id we want to use
//! as a stable correlation handle so the WS client can later send back a
//! `setVerdict` referencing the same row.

use crate::ws_messages::ConnectionRow;
use snitchwatch_proto::protocol::Connection;

pub const ASK_ROW_PREFIX: &str = "ask-";

pub fn ask_row_id(notification_id: u64) -> String {
    format!("{ASK_ROW_PREFIX}{notification_id}")
}

pub fn connection_to_row(_conn: &Connection, _notification_id: u64) -> ConnectionRow {
    unimplemented!("task 1 step 4 — write the implementation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            src_ip: "192.168.1.10".to_string(),
            src_port: 51544,
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            user_id: 1000,
            process_id: 4242,
            process_path: "/usr/bin/curl".to_string(),
            process_cwd: "/home/alice".to_string(),
            process_args: vec!["curl".into(), "https://github.com".into()],
            process_env: Default::default(),
            process_checksums: Default::default(),
            process_tree: vec![],
        }
    }

    #[test]
    fn ask_row_id_is_stable() {
        assert_eq!(ask_row_id(7), "ask-7");
    }

    #[test]
    fn connection_to_row_populates_all_visible_fields() {
        let conn = sample_connection();
        let row = connection_to_row(&conn, 42);

        assert_eq!(row.id, "ask-42");
        assert_eq!(row.process, "curl");
        assert_eq!(row.process_path.as_deref(), Some("/usr/bin/curl"));
        assert_eq!(row.dst_host, "github.com");
        assert_eq!(row.dst_ip, "140.82.121.4");
        assert_eq!(row.dst_port, 443);
        assert_eq!(row.protocol, "tcp");
        assert_eq!(row.direction, "outgoing");
        assert!(row.action.is_none(), "ask-rule rows start pending");
        assert_eq!(row.bytes_sent, 0);
        assert_eq!(row.bytes_received, 0);
    }

    #[test]
    fn connection_with_no_dst_host_falls_back_to_ip() {
        let mut conn = sample_connection();
        conn.dst_host = String::new();
        let row = connection_to_row(&conn, 5);
        assert_eq!(row.dst_host, "140.82.121.4");
    }

    #[test]
    fn process_basename_is_extracted_from_path() {
        let mut conn = sample_connection();
        conn.process_path = "/opt/firefox/firefox".to_string();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "firefox");
    }

    #[test]
    fn process_with_empty_path_uses_unknown() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "<unknown>");
        assert_eq!(row.process_path, None);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p snitchwatch-bridge translator::connection`
Expected: PANIC with `unimplemented!("task 1 step 4 ...")` on each non-trivial test (the `ask_row_id_is_stable` test passes).

- [ ] **Step 4: Replace the stub with the implementation**

Edit `crates/snitchwatch-bridge/src/translator/connection.rs` and replace the `connection_to_row` body:

```rust
pub fn connection_to_row(conn: &Connection, notification_id: u64) -> ConnectionRow {
    let process = if conn.process_path.is_empty() {
        "<unknown>".to_string()
    } else {
        std::path::Path::new(&conn.process_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string()
    };

    let process_path = if conn.process_path.is_empty() {
        None
    } else {
        Some(conn.process_path.clone())
    };

    let dst_host = if conn.dst_host.is_empty() {
        conn.dst_ip.clone()
    } else {
        conn.dst_host.clone()
    };

    ConnectionRow {
        id: ask_row_id(notification_id),
        process,
        process_path,
        dst_host,
        dst_ip: conn.dst_ip.clone(),
        dst_port: conn.dst_port as u16,
        protocol: conn.protocol.clone(),
        direction: "outgoing".to_string(),
        action: None,
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p snitchwatch-bridge translator::connection`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/connection.rs \
        crates/snitchwatch-bridge/src/translator/mod.rs
git commit -m "feat(bridge): add Connection→ConnectionRow translator for AskRule unary RPC"
```

---

### Task 2: `verdict_to_rule` — Verdict + Connection → Rule

**Files:**
- Create: `crates/snitchwatch-bridge/src/translator/verdict.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/mod.rs`

- [ ] **Step 1: Add the module declaration**

Edit `crates/snitchwatch-bridge/src/translator/mod.rs` and add:

```rust
pub mod verdict;
```

- [ ] **Step 2: Write the failing test file**

Create `crates/snitchwatch-bridge/src/translator/verdict.rs`:

```rust
//! Translate a user-supplied `Verdict` (allow / deny) into the `Rule` proto
//! shape opensnitchd expects as the `AskRule` reply.
//!
//! The M0 spike taught us three things about `Rule`:
//!   - `name` must be non-empty (the daemon rejects empty names).
//!   - `created` is a unix-seconds int64.
//!   - `duration: "once"` means "this connection only" — the daemon does not
//!     persist the rule. Higher milestones (M2+) replace this with proper
//!     scope/remember handling.

use crate::cache::connections::Verdict;
use snitchwatch_proto::protocol::{Connection, Rule};

pub fn verdict_to_rule(verdict: Verdict, conn: &Connection, now_secs: i64) -> Rule {
    let _ = (verdict, conn, now_secs);
    unimplemented!("task 2 step 4 — write the implementation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            process_path: "/usr/bin/curl".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn allow_verdict_produces_allow_rule_with_once_duration() {
        let rule = verdict_to_rule(Verdict::Allow, &sample_connection(), 1_700_000_000);
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(rule.enabled);
        assert_eq!(rule.created, 1_700_000_000);
        assert!(!rule.name.is_empty(), "daemon rejects empty rule names");
        assert!(rule.name.contains("allow"));
    }

    #[test]
    fn deny_verdict_produces_deny_rule() {
        let rule = verdict_to_rule(Verdict::Deny, &sample_connection(), 1_700_000_000);
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "once");
        assert!(rule.name.contains("deny"));
    }

    #[test]
    fn rule_name_includes_remote_host_for_traceability() {
        let rule = verdict_to_rule(Verdict::Allow, &sample_connection(), 0);
        assert!(rule.name.contains("github.com"), "got: {}", rule.name);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p snitchwatch-bridge translator::verdict`
Expected: PANIC with `unimplemented!("task 2 step 4 ...")` on all three tests.

- [ ] **Step 4: Replace the stub with the implementation**

Edit `crates/snitchwatch-bridge/src/translator/verdict.rs` and replace the body of `verdict_to_rule`:

```rust
pub fn verdict_to_rule(verdict: Verdict, conn: &Connection, now_secs: i64) -> Rule {
    let action = match verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
    };

    let host = if conn.dst_host.is_empty() {
        conn.dst_ip.as_str()
    } else {
        conn.dst_host.as_str()
    };

    Rule {
        created: now_secs,
        name: format!("snitchwatch-{action}-{host}-{}", conn.dst_port),
        description: "snitchwatch interactive verdict".to_string(),
        enabled: true,
        precedence: false,
        nolog: false,
        action: action.to_string(),
        duration: "once".to_string(),
        operator: None,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p snitchwatch-bridge translator::verdict`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/verdict.rs \
        crates/snitchwatch-bridge/src/translator/mod.rs
git commit -m "feat(bridge): add Verdict→Rule translator for AskRule replies"
```

---

## Part B — `UiService` gRPC server

### Task 3: Skeleton `UiService` with all 5 RPCs (no AskRule logic yet)

**Files:**
- Create: `crates/snitchwatch-bridge/src/grpc_server.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Modify: `crates/snitchwatch-bridge/Cargo.toml`

- [ ] **Step 1: Add `async-stream` to the bridge crate dependencies**

Edit `crates/snitchwatch-bridge/Cargo.toml`. Add the line under `[dependencies]`:

```toml
async-stream = { workspace = true }
```

- [ ] **Step 2: Add the module declaration**

Edit `crates/snitchwatch-bridge/src/lib.rs`. Replace the line `pub mod grpc_client;` with:

```rust
pub mod grpc_server;
```

(If `grpc_client` was already removed in a prior step, just add the new line.)

- [ ] **Step 3: Write the failing test for the skeleton**

Create `crates/snitchwatch-bridge/src/grpc_server.rs`:

```rust
//! Bridge-side gRPC server: implements `protocol.UI` and is dialed by
//! opensnitchd as the gRPC client.
//!
//! Replaces the M1 dial-out flow that lived in the now-deleted
//! `grpc_client.rs` and `translator/downstream.rs` envelope hack.

use crate::cache::connections::ConnectionCache;
use crate::ws_messages::ServerMessage;
use snitchwatch_proto::protocol::ui_server::{Ui, UiServer};
use snitchwatch_proto::protocol::{
    Alert, ClientConfig, Connection, MsgResponse, Notification, NotificationReply, PingReply,
    PingRequest, Rule,
};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio_stream::Stream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

/// Bridge-side gRPC server state. Handed to `UiServer::new` for tonic.
#[derive(Clone)]
pub struct UiService {
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
    ) -> Self {
        Self { cache, broadcast }
    }

    /// Convenience: wrap into a tonic `UiServer<UiService>` ready for
    /// `Server::builder().add_service(...)`.
    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }
}

#[tonic::async_trait]
impl Ui for UiService {
    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let id = request.into_inner().id;
        Ok(Response::new(PingReply { id }))
    }

    async fn ask_rule(&self, _request: Request<Connection>) -> Result<Response<Rule>, Status> {
        Err(Status::unimplemented("task 4 — wire AskRule pending flow"))
    }

    async fn subscribe(
        &self,
        request: Request<ClientConfig>,
    ) -> Result<Response<ClientConfig>, Status> {
        let cfg = request.into_inner();
        info!(client = %cfg.name, version = %cfg.version, "client subscribed");
        Ok(Response::new(cfg))
    }

    async fn post_alert(&self, request: Request<Alert>) -> Result<Response<MsgResponse>, Status> {
        let alert = request.into_inner();
        info!(id = alert.id, type_ = alert.r#type, "alert received");
        Ok(Response::new(MsgResponse { id: alert.id }))
    }

    type NotificationsStream =
        Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send + 'static>>;

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        info!("notifications stream opened");
        let mut inbound = request.into_inner();
        tokio::spawn(async move {
            while let Ok(Some(reply)) = inbound.message().await {
                info!(id = reply.id, code = reply.code, "notification reply from daemon");
            }
            warn!("notification reply stream ended");
        });

        let outbound = async_stream::try_stream! {
            // Hold the stream open with no commands until M3+ wires up
            // config-push from the GUI side.
            let () = std::future::pending().await;
            yield Notification::default();
        };

        Ok(Response::new(
            Box::pin(outbound) as Self::NotificationsStream
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snitchwatch_proto::protocol::ui_client::UiClient;
    use std::time::Duration;
    use tokio::sync::{broadcast, Mutex};
    use tonic::transport::Server;

    async fn spawn_test_service() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel(16);
        let svc = UiService::new(cache, tx).into_server();

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
    async fn ping_round_trips_id() {
        let addr = spawn_test_service().await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);
        let reply = client
            .ping(PingRequest { id: 99, stats: None })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(reply.id, 99);
    }

    #[tokio::test]
    async fn subscribe_echoes_config() {
        let addr = spawn_test_service().await;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);
        let cfg = ClientConfig {
            id: 1,
            name: "opensnitchd-test".to_string(),
            version: "1.6.0".to_string(),
            ..Default::default()
        };
        let echoed = client.subscribe(cfg.clone()).await.unwrap().into_inner();
        assert_eq!(echoed.name, cfg.name);
        assert_eq!(echoed.version, cfg.version);
    }
}
```

- [ ] **Step 4: Run the test — it should pass after a clean build**

Run: `cargo test -p snitchwatch-bridge grpc_server::tests`
Expected: 2 passed (`ping_round_trips_id`, `subscribe_echoes_config`).

If the build fails because `grpc_client` is still referenced from `lib.rs`, delete the `pub mod grpc_client;` line — task 10 will rm the file.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_server.rs \
        crates/snitchwatch-bridge/src/lib.rs \
        crates/snitchwatch-bridge/Cargo.toml
git commit -m "feat(bridge): add UiService gRPC server skeleton (Ping/Subscribe/PostAlert/Notifications)"
```

---

### Task 4: Wire `ask_rule` to the pending-cache + verdict oneshot

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_server.rs`

- [ ] **Step 1: Write the failing test for the round trip**

Append to the `tests` module in `crates/snitchwatch-bridge/src/grpc_server.rs`:

```rust
    use crate::cache::connections::Verdict;
    use crate::translator::connection::ask_row_id;

    #[tokio::test]
    async fn ask_rule_blocks_until_cache_resolves_with_allow() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = UiService::new(cache.clone(), tx).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);

        // Spawn the AskRule call in the background — it must block until we
        // resolve the row from the test.
        let ask_handle = tokio::spawn(async move {
            client
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "example.com".into(),
                    dst_ip: "93.184.216.34".into(),
                    dst_port: 443,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
        });

        // Wait for the broadcast that says "row inserted as pending".
        let inserted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("ask_rule did not broadcast")
            .expect("broadcast error");
        let row_id = match inserted {
            ServerMessage::InsertConnectionRows { rows } => rows[0].id.clone(),
            other => panic!("expected InsertConnectionRows, got {other:?}"),
        };
        // Should match the deterministic ask_row_id since the daemon-supplied
        // notification id is the AskRule sequence number — for the very first
        // call we expect ask-1.
        assert_eq!(row_id, ask_row_id(1));

        // Resolve the pending row with Allow.
        cache.lock().await.resolve(&row_id, Verdict::Allow).unwrap();

        // The AskRule call should now return a Rule with action=allow.
        let rule = ask_handle.await.unwrap().unwrap().into_inner();
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(!rule.name.is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p snitchwatch-bridge grpc_server::tests::ask_rule_blocks_until_cache_resolves_with_allow`
Expected: FAIL with `Status: unimplemented "task 4 — wire AskRule pending flow"`.

- [ ] **Step 3: Add the AskRule sequence counter field**

Edit `crates/snitchwatch-bridge/src/grpc_server.rs`. Replace the `UiService` struct + `new` constructor:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub struct UiService {
    cache: Arc<Mutex<ConnectionCache>>,
    broadcast: broadcast::Sender<ServerMessage>,
    next_ask_id: Arc<AtomicU64>,
}

impl UiService {
    pub fn new(
        cache: Arc<Mutex<ConnectionCache>>,
        broadcast: broadcast::Sender<ServerMessage>,
    ) -> Self {
        Self {
            cache,
            broadcast,
            next_ask_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }
}
```

- [ ] **Step 4: Implement the `ask_rule` body**

Add the imports near the top of the file:

```rust
use crate::translator::connection::connection_to_row;
use crate::translator::verdict::verdict_to_rule;
```

Replace the existing stub `ask_rule` method body with:

```rust
    async fn ask_rule(&self, request: Request<Connection>) -> Result<Response<Rule>, Status> {
        let conn = request.into_inner();
        let ask_id = self.next_ask_id.fetch_add(1, Ordering::Relaxed);

        // 1. Build the row and insert as pending. The cache returns the
        //    receiver side of a oneshot the WS layer fires on `setVerdict`.
        let row = connection_to_row(&conn, ask_id);
        let verdict_rx = {
            let mut cache = self.cache.lock().await;
            cache.insert_pending(row.clone())
        };

        // 2. Broadcast to any connected WS clients so the UI shows it
        //    immediately. A send error means there are no subscribers right
        //    now (the row is still pending in the cache, so a future client
        //    that calls Subscribe will pick it up).
        if self.broadcast.receiver_count() > 0 {
            let msg = ServerMessage::InsertConnectionRows { rows: vec![row] };
            if let Err(e) = self.broadcast.send(msg) {
                warn!(error = %e, "broadcast send failed");
            }
        }

        // 3. Block this RPC handler until the user decides. opensnitchd is
        //    happy to wait — it streams the connection to userspace and
        //    holds the packet until we reply.
        let verdict = verdict_rx.await.map_err(|_canceled| {
            Status::cancelled("verdict oneshot dropped before resolution")
        })?;

        // 4. Translate the verdict into a Rule reply.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Response::new(verdict_to_rule(verdict, &conn, now_secs)))
    }
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p snitchwatch-bridge grpc_server::tests::ask_rule_blocks_until_cache_resolves_with_allow`
Expected: PASS.

Then run the whole grpc_server test suite to make sure nothing regressed:
Run: `cargo test -p snitchwatch-bridge grpc_server`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_server.rs
git commit -m "feat(bridge): wire AskRule unary handler through pending-cache + verdict oneshot"
```

---

### Task 5: Test that AskRule deny path also works

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_server.rs`

This task adds a second integration test that exercises the deny branch of `verdict_to_rule` end-to-end via the gRPC server, plus a test that two concurrent AskRules get different ask_ids.

- [ ] **Step 1: Add the failing tests**

Append to the `tests` module:

```rust
    #[tokio::test]
    async fn ask_rule_returns_deny_rule_when_resolved_with_deny() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, _rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = UiService::new(cache.clone(), tx).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut client = UiClient::new(channel);

        let ask_handle = tokio::spawn(async move {
            client
                .ask_rule(Connection {
                    protocol: "tcp".into(),
                    dst_host: "tracker.example.com".into(),
                    dst_ip: "1.2.3.4".into(),
                    dst_port: 80,
                    process_path: "/usr/bin/curl".into(),
                    ..Default::default()
                })
                .await
        });

        // Resolve with deny once the row exists (poll the cache briefly).
        let row_id = ask_row_id(1);
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if cache.lock().await.resolve(&row_id, Verdict::Deny).is_ok() {
                break;
            }
        }

        let rule = ask_handle.await.unwrap().unwrap().into_inner();
        assert_eq!(rule.action, "deny");
    }

    #[tokio::test]
    async fn two_concurrent_ask_rules_get_distinct_ask_ids() {
        let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
        let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = UiService::new(cache.clone(), tx).into_server();
        tokio::spawn(async move {
            Server::builder()
                .add_service(svc)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .ok();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Spawn two ask_rule calls and watch the broadcast for two distinct
        // row ids.
        for _ in 0..2 {
            let endpoint = format!("http://{addr}");
            tokio::spawn(async move {
                let channel = tonic::transport::Endpoint::from_shared(endpoint)
                    .unwrap()
                    .connect()
                    .await
                    .unwrap();
                let mut client = UiClient::new(channel);
                let _ = client.ask_rule(Connection::default()).await;
            });
        }

        let mut seen = std::collections::HashSet::new();
        while seen.len() < 2 {
            let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("missed broadcast")
                .expect("broadcast error");
            if let ServerMessage::InsertConnectionRows { rows } = msg {
                for r in rows {
                    seen.insert(r.id);
                }
            }
        }
        assert!(seen.contains(&ask_row_id(1)));
        assert!(seen.contains(&ask_row_id(2)));

        // Drain pending so the spawned tasks don't hang the test runtime.
        let _ = cache.lock().await.resolve(&ask_row_id(1), Verdict::Deny);
        let _ = cache.lock().await.resolve(&ask_row_id(2), Verdict::Deny);
    }
```

- [ ] **Step 2: Run the test to verify both pass**

Run: `cargo test -p snitchwatch-bridge grpc_server`
Expected: 5 passed.

Both tests should already pass — task 4 implemented the full deny path, and the atomic counter guarantees distinct ids.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_server.rs
git commit -m "test(bridge): cover AskRule deny path and concurrent-ask id allocation"
```

---

## Part C — Replumb the bridge orchestrator

### Task 6: `BridgeConfig` + `run` bind a gRPC server instead of dialing

**Files:**
- Modify: `crates/snitchwatch-bridge-cli/src/lib.rs`

- [ ] **Step 1: Replace the imports and `BridgeConfig`**

Edit `crates/snitchwatch-bridge-cli/src/lib.rs`. Replace the entire imports block + `BridgeConfig` + `from_env` with:

```rust
use anyhow::{Context, Result};
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::grpc_server::UiService;
use snitchwatch_bridge::translator::upstream;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tonic::transport::Server;
use tracing::{error, info};

/// Runtime configuration for [`run`].
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Address to bind the gRPC `Ui` server on. opensnitchd will dial this.
    /// Use port `0` for ephemeral.
    pub grpc_bind: SocketAddr,
    /// Address to bind the WebSocket server on. Use port `0` for ephemeral.
    pub ws_bind: SocketAddr,
    /// Cache capacity (number of recent rows retained).
    pub cache_capacity: usize,
}

impl BridgeConfig {
    pub fn from_env() -> Result<Self> {
        let grpc_bind_str = std::env::var("SNITCHWATCH_GRPC_BIND")
            .unwrap_or_else(|_| "127.0.0.1:0".to_string());
        let grpc_bind: SocketAddr = grpc_bind_str
            .parse()
            .with_context(|| format!("invalid SNITCHWATCH_GRPC_BIND: {grpc_bind_str}"))?;

        let ws_bind_str =
            std::env::var("SNITCHWATCH_WS_BIND").unwrap_or_else(|_| "127.0.0.1:0".to_string());
        let ws_bind: SocketAddr = ws_bind_str
            .parse()
            .with_context(|| format!("invalid SNITCHWATCH_WS_BIND: {ws_bind_str}"))?;

        Ok(Self {
            grpc_bind,
            ws_bind,
            cache_capacity: 10_000,
        })
    }
}
```

- [ ] **Step 2: Replace `RunningBridge` with the dual-address version**

Replace the `RunningBridge` struct + impl in the same file:

```rust
/// Handle to a running bridge. Dropping this does **not** shut the bridge
/// down — call [`RunningBridge::shutdown`] explicitly when you're done.
pub struct RunningBridge {
    /// Actual bound WebSocket address.
    pub ws_addr: SocketAddr,
    /// Actual bound gRPC address (so callers who passed `:0` can discover it).
    pub grpc_addr: SocketAddr,
    ws_shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
}

impl RunningBridge {
    /// Signal every background task to stop. Safe to call more than once.
    pub fn shutdown(mut self) {
        if let Some(tx) = self.ws_shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = self.grpc_shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
```

- [ ] **Step 3: Replace the `run` function**

Replace the entire `run` function (and delete `downstream_pump` and `verdict_to_json`):

```rust
/// Start the bridge and return as soon as every background task is running.
///
/// Both the WebSocket server and the gRPC `Ui` server are bound and accepting
/// connections by the time this returns. opensnitchd can dial in immediately.
pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
    info!(?config, "starting snitchwatch-bridge");

    // Channels between the WS server and the orchestrator.
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Shared connection cache (pending-row state + decided-row history).
    let cache = Arc::new(Mutex::new(ConnectionCache::new(config.cache_capacity)));

    // --- WebSocket server ---------------------------------------------------
    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
    };
    let ws_server = WsServer::new(config.ws_bind, ws_handles);
    let (ws_listener, ws_addr) = ws_server
        .bind()
        .await
        .context("failed to bind WebSocket listener")?;
    let (ws_shutdown_tx, ws_shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            res = ws_server.serve(ws_listener) => {
                if let Err(e) = res {
                    error!(error = %e, "ws_server::serve exited");
                }
            }
            _ = ws_shutdown_rx => {
                info!("ws server shutdown signal received");
            }
        }
    });

    // --- gRPC Ui server -----------------------------------------------------
    let grpc_listener = tokio::net::TcpListener::bind(config.grpc_bind)
        .await
        .with_context(|| format!("failed to bind gRPC listener on {}", config.grpc_bind))?;
    let grpc_addr = grpc_listener
        .local_addr()
        .context("gRPC listener has no local address")?;

    let ui_service = UiService::new(cache.clone(), broadcast_tx.clone()).into_server();
    let (grpc_shutdown_tx, grpc_shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
        let serve = Server::builder()
            .add_service(ui_service)
            .serve_with_incoming_shutdown(incoming, async {
                let _ = grpc_shutdown_rx.await;
            });
        if let Err(e) = serve.await {
            error!(error = %e, "grpc Ui server exited");
        } else {
            info!("grpc Ui server shutdown signal received");
        }
    });

    // --- Upstream pump: WS client messages → cache (→ oneshot resolve) -------
    let cache_for_upstream = cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let mut cache = cache_for_upstream.lock().await;
            match upstream::apply(&mut cache, msg) {
                Ok(effect) => info!(?effect, "applied upstream effect"),
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    Ok(RunningBridge {
        ws_addr,
        grpc_addr,
        ws_shutdown_tx: Some(ws_shutdown_tx),
        grpc_shutdown_tx: Some(grpc_shutdown_tx),
    })
}
```

- [ ] **Step 4: Delete the now-dead unit tests**

In the same file, replace the `#[cfg(test)] mod tests { ... }` block with a single placeholder so we don't lose the module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_binds_both_ports_and_shutdown_works() {
        let cfg = BridgeConfig {
            grpc_bind: "127.0.0.1:0".parse().unwrap(),
            ws_bind: "127.0.0.1:0".parse().unwrap(),
            cache_capacity: 64,
        };
        let bridge = run(cfg).await.expect("run failed");
        assert!(bridge.ws_addr.port() != 0);
        assert!(bridge.grpc_addr.port() != 0);
        bridge.shutdown();
    }
}
```

- [ ] **Step 5: Build to verify the new orchestrator compiles**

Run: `cargo check -p snitchwatch-bridge-cli`
Expected: clean build (it will fail if `grpc_client` or `downstream` are still re-exported from `snitchwatch-bridge`; task 10 cleans those up — for now, if the build fails because `mod downstream;` still exists, do NOT delete it yet, just stop here and proceed to Task 10 first, then come back and re-run this step).

Run: `cargo test -p snitchwatch-bridge-cli run_binds_both_ports_and_shutdown_works`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/snitchwatch-bridge-cli/src/lib.rs
git commit -m "refactor(bridge-cli): bind gRPC Ui server instead of dialing opensnitchd"
```

---

### Task 7: Update `bridge-cli/src/main.rs` to print `GRPC_LISTEN_ADDR`

**Files:**
- Modify: `crates/snitchwatch-bridge-cli/src/main.rs`

- [ ] **Step 1: Update the module docstring**

Replace the doc comment block at the top of `crates/snitchwatch-bridge-cli/src/main.rs`:

```rust
//! Bridge CLI — runs the bridge that exposes a gRPC `Ui` server (which
//! opensnitchd dials in to) and a WebSocket server for the GUI front-end.
//!
//! Usage:
//!   snitchwatch-bridge-cli
//!
//! Env vars (all optional):
//!   SNITCHWATCH_GRPC_BIND  gRPC bind address (default: 127.0.0.1:0)
//!   SNITCHWATCH_WS_BIND    WebSocket bind address (default: 127.0.0.1:0)
//!
//! On startup the CLI prints two machine-parseable lines to stdout so test
//! harnesses and wrapping processes can discover the ports:
//!
//!   GRPC_LISTEN_ADDR=<addr>
//!   WS_LISTEN_ADDR=<addr>
//!
//! All of the orchestration logic lives in `snitchwatch_bridge_cli::run` so
//! integration tests can exercise it without spawning a subprocess.
```

- [ ] **Step 2: Add the `GRPC_LISTEN_ADDR` print line**

Replace the line `println!("WS_LISTEN_ADDR={}", bridge.ws_addr);` with:

```rust
    // Machine-parseable lines for test harnesses. Order matters: opensnitchd
    // wrappers grep for GRPC_LISTEN_ADDR first.
    println!("GRPC_LISTEN_ADDR={}", bridge.grpc_addr);
    println!("WS_LISTEN_ADDR={}", bridge.ws_addr);
```

- [ ] **Step 3: Build to verify the CLI still compiles**

Run: `cargo check -p snitchwatch-bridge-cli --bin snitchwatch-bridge-cli`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-bridge-cli/src/main.rs
git commit -m "feat(bridge-cli): print GRPC_LISTEN_ADDR alongside WS_LISTEN_ADDR"
```

---

## Part D — Mock + integration test

### Task 8: Rewrite `tests/mock_opensnitchd` as a tonic client

**Files:**
- Modify: `tests/mock_opensnitchd/src/lib.rs`

- [ ] **Step 1: Replace the entire file**

Overwrite `tests/mock_opensnitchd/src/lib.rs` with:

```rust
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

/// Errors the mock can surface to tests.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    #[error("connect failed: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("rpc failed: {0}")]
    Rpc(#[from] tonic::Status),
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
        // Brief retry loop in case the bridge is still binding.
        let endpoint = Endpoint::from_shared(format!("http://{addr}"))
            .map_err(|e| MockError::Connect(e))?
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

    /// Send a single AskRule unary RPC and wait for the bridge's `Rule` reply.
    /// This blocks until the GUI user resolves the pending row.
    pub async fn ask_rule(&mut self, conn: Connection) -> Result<Rule, MockError> {
        let rule = self.client.ask_rule(conn).await?.into_inner();
        Ok(rule)
    }

    pub async fn post_alert(&mut self, alert: Alert) -> Result<MsgResponse, MockError> {
        let reply = self.client.post_alert(alert).await?.into_inner();
        Ok(reply)
    }

    /// Open the bidi `Notifications` stream. The returned sender pushes
    /// `NotificationReply` messages upstream; the bridge's outbound
    /// `Notification` stream is consumed by a background task that simply
    /// counts how many notifications it received (exposed via the returned
    /// `mpsc::Receiver<u64>` which fires whenever a Notification arrives).
    pub async fn open_notifications(
        &mut self,
    ) -> Result<(mpsc::Sender<NotificationReply>, mpsc::Receiver<u64>), MockError> {
        let (reply_tx, reply_rx) = mpsc::channel::<NotificationReply>(16);
        let outbound = tokio_stream::wrappers::ReceiverStream::new(reply_rx);

        let mut inbound = self
            .client
            .notifications(outbound)
            .await?
            .into_inner();

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
        let svc = UiService::new(cache, tx).into_server();
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
}
```

- [ ] **Step 2: Update the mock_opensnitchd Cargo.toml dependency on bridge**

Edit `tests/mock_opensnitchd/Cargo.toml`. The crate already depends on `snitchwatch-proto`; add a dev-dependency on `snitchwatch-bridge` if it isn't already there. Add or confirm:

```toml
[dependencies]
snitchwatch-proto = { path = "../../crates/snitchwatch-proto" }
tokio = { workspace = true }
tonic = { workspace = true }
prost = { workspace = true }
tokio-stream = "0.1"
thiserror = { workspace = true }

[dev-dependencies]
snitchwatch-bridge = { path = "../../crates/snitchwatch-bridge" }
```

- [ ] **Step 3: Run the mock's own tests**

Run: `cargo test -p mock_opensnitchd`
Expected: 2 passed (`mock_can_ping_bridge`, `mock_can_subscribe_to_bridge`).

- [ ] **Step 4: Commit**

```bash
git add tests/mock_opensnitchd/src/lib.rs tests/mock_opensnitchd/Cargo.toml
git commit -m "refactor(mock): rewrite mock_opensnitchd as gRPC client matching real topology"
```

---

### Task 9: Update `tests/bridge_protocol_test.rs` for the unary AskRule round-trip

**Files:**
- Modify: `tests/bridge_protocol_test.rs`

- [ ] **Step 1: Replace the test file**

Overwrite `tests/bridge_protocol_test.rs` with:

```rust
//! End-to-end test: mock_opensnitchd (gRPC client) ↔ bridge (gRPC server +
//! WS server) ↔ WebSocket UI client.
//!
//! This is the M1.5 acceptance test. It proves that:
//!   1. The bridge binds both its gRPC and WebSocket ports.
//!   2. opensnitchd (mocked) can dial in and call `AskRule`.
//!   3. The bridge broadcasts an `InsertConnectionRows` to any connected WS
//!      client carrying the new pending row.
//!   4. A WS `setVerdict` resolves the pending row, and the original
//!      `AskRule` unary call returns a `Rule` whose `action` matches.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn ask_rule_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Boot the bridge with both ephemeral ports.
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    // 2. Connect a WebSocket client BEFORE the AskRule call so we don't miss
    //    the broadcast.
    let ws_url = format!("ws://{}/stream", bridge.ws_addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect failed");

    // 3. Spawn an opensnitchd mock client and fire AskRule in the background.
    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "tcp".into(),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // 4. Wait for the InsertConnectionRows broadcast on the WS.
    let insert_msg = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value =
                        serde_json::from_str(&t).expect("server sent bad json");
                    if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                        break v;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for insertConnectionRows");

    let rows = insert_msg
        .get("rows")
        .and_then(|r| r.as_array())
        .expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let row_id = row
        .get("id")
        .and_then(|v| v.as_str())
        .expect("row id")
        .to_string();
    assert_eq!(row.get("process").and_then(|v| v.as_str()), Some("curl"));
    assert_eq!(
        row.get("dstHost").and_then(|v| v.as_str()),
        Some("example.com")
    );
    assert_eq!(row.get("dstPort").and_then(|v| v.as_u64()), Some(443));
    assert!(
        row.get("action").map(|v| v.is_null()).unwrap_or(false),
        "pending rows must have action: null"
    );

    // 5. Send a SetVerdict to decide the pending row.
    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "allow",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string()))
        .await
        .expect("ws send failed");

    // 6. The mock's AskRule call should now return with action=allow.
    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .expect("ask_rule timed out")
        .expect("ask_rule task panicked");
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(!rule.name.is_empty(), "daemon rejects empty rule names");

    bridge.shutdown();
}

#[tokio::test]
async fn deny_round_trip_unary() {
    let _ = tracing_subscriber::fmt::try_init();

    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_bind: "127.0.0.1:0".parse().unwrap(),
        cache_capacity: 1024,
    };
    let bridge = run(cfg).await.expect("bridge run failed");

    let ws_url = format!("ws://{}/stream", bridge.ws_addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect failed");

    let grpc_addr = bridge.grpc_addr;
    let ask_handle = tokio::spawn(async move {
        let mut mock = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        mock.ask_rule(Connection {
            protocol: "udp".into(),
            dst_host: "tracker.bad".into(),
            dst_ip: "1.2.3.4".into(),
            dst_port: 53,
            process_path: "/usr/bin/dnsmasq".into(),
            ..Default::default()
        })
        .await
        .unwrap()
    });

    // Drain WS until we see the insert.
    let row_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v.get("action").and_then(|a| a.as_str()) == Some("insertConnectionRows") {
                        return v["rows"][0]["id"].as_str().unwrap().to_string();
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();

    let verdict = json!({
        "action": "setVerdict",
        "rowId": row_id,
        "verdict": "deny",
        "scope": "this_host",
        "remember": false,
    });
    ws.send(Message::Text(verdict.to_string())).await.unwrap();

    let rule = tokio::time::timeout(Duration::from_secs(5), ask_handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rule.action, "deny");

    bridge.shutdown();
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test --test bridge_protocol_test`
Expected: 2 passed.

If `tests/integration` workspace member references the old test name, no action — it's a separate folder. The `bridge_protocol_test.rs` lives at the workspace root under `tests/`.

- [ ] **Step 3: Commit**

```bash
git add tests/bridge_protocol_test.rs
git commit -m "test(integration): rewrite AskRule round-trip as unary RPC instead of envelope"
```

---

## Part E — Cleanup

### Task 10: Delete the M1 envelope code and `grpc_client.rs`

**Files:**
- Delete: `crates/snitchwatch-bridge/src/grpc_client.rs`
- Delete: `crates/snitchwatch-bridge/src/translator/downstream.rs`
- Modify: `crates/snitchwatch-bridge/src/lib.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/mod.rs`
- Modify: `crates/snitchwatch-bridge/src/ws_messages.rs`

- [ ] **Step 1: Delete the dead source files**

Run:

```bash
rm crates/snitchwatch-bridge/src/grpc_client.rs
rm crates/snitchwatch-bridge/src/translator/downstream.rs
```

- [ ] **Step 2: Strip module declarations**

Edit `crates/snitchwatch-bridge/src/lib.rs` and remove any line that says `pub mod grpc_client;` (it should already be gone after task 3 step 2; this is a belt-and-braces check).

Edit `crates/snitchwatch-bridge/src/translator/mod.rs` and remove the line `pub mod downstream;`.

- [ ] **Step 3: Drop the now-stale TODO note in `ws_messages.rs`**

Open `crates/snitchwatch-bridge/src/ws_messages.rs`. Find the `TODO(task-7-followup)` comment near the top of the file (around line 6). Delete the entire comment block — the M2 envelope contract no longer exists, so the TODO is dead.

- [ ] **Step 4: Verify nothing references the deleted symbols**

Run: `cargo check --workspace --all-targets`
Expected: clean build. If a `downstream::translate_notification` reference remains, fix it (it should only have lived in `bridge-cli/src/lib.rs`, which task 6 already replaced).

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add -A crates/snitchwatch-bridge/src/grpc_client.rs \
          crates/snitchwatch-bridge/src/translator/downstream.rs \
          crates/snitchwatch-bridge/src/lib.rs \
          crates/snitchwatch-bridge/src/translator/mod.rs \
          crates/snitchwatch-bridge/src/ws_messages.rs
git commit -m "refactor(bridge): delete grpc_client.rs and downstream envelope translator"
```

---

### Task 11: Polish — README, design spec, m0-findings, `just check`

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`
- Modify: `docs/m0-spike-findings.md`

- [ ] **Step 1: Update the README architecture section**

Open `README.md`. Find any line/paragraph mentioning "the bridge dials opensnitchd" and replace it with the inverted-topology description. Use this exact wording so the README stays consistent with the spec:

```markdown
## Architecture

The bridge is a Rust workspace member that exposes two server sockets on
loopback:

- **gRPC `protocol.UI` server** — opensnitchd dials in here as the gRPC
  client. The bridge implements `Ping`, `AskRule`, `Subscribe`, `PostAlert`,
  and the bidi `Notifications` stream. `AskRule` is a blocking unary handler:
  the bridge inserts a pending row into its in-memory cache, broadcasts it on
  the WebSocket, awaits the user verdict via a `oneshot`, then translates the
  verdict into a `Rule` reply.
- **WebSocket server** — the front-end (vendored LS-for-Linux UI in M2,
  Tauri shell in M3) connects to `/stream` and exchanges Little Snitch v6
  protocol messages with the bridge.

opensnitchd's `Server.Address` config tells it where to dial. The bridge
publishes both bound addresses on stdout at startup as `GRPC_LISTEN_ADDR=...`
and `WS_LISTEN_ADDR=...`.
```

If the README has a "Running the bridge against real opensnitchd" section, update it to reflect that opensnitchd's `default-config.json` field `Server.Address` should be set to the bridge's `GRPC_LISTEN_ADDR` (e.g. `127.0.0.1:50051`).

- [ ] **Step 2: Update the design spec milestone table**

Open `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`. Find the "Milestones" or "Phasing" table and:

1. Mark M0 (Spike) as **DONE** with the M0 commit range (`1acd100..a6baa15`).
2. Mark M1 (Bridge foundation) as **DONE** at commit `a54c0b4`.
3. Insert a new row after M1: `M1.5 | Topology Flip | DONE | inverts gRPC topology to match real opensnitchd protocol; deletes JSON envelope; mock becomes client.`
4. Leave M2-M6 status fields untouched (still planned).

- [ ] **Step 3: Append the "Adjustment applied" footer to m0-findings**

Open `docs/m0-spike-findings.md` and append a new section at the bottom:

```markdown
---

## Adjustment applied

The topology correction documented in this spike was implemented in Plan 2
("M1.5 — Topology Flip"). The bridge now binds the gRPC `Ui` server and
opensnitchd dials in as the gRPC client. The M1 JSON envelope inside
`Notification.data` and the `grpc_client.rs` reconnect helper have been
deleted. See `crates/snitchwatch-bridge/src/grpc_server.rs` and
`docs/superpowers/plans/2026-04-11-topology-flip.md`.
```

- [ ] **Step 4: Run the full check pipeline**

Run: `just check`
Expected: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace` all green.

If clippy fires `clippy::large_enum_variant` on anything that carries a `ConnectionRow`, box it (per memory `clippy_gotchas_bridge.md`). If clippy fires `clippy::let_underscore_future` on a discarded `oneshot::Receiver`, replace `let _ = ...` with `drop(...)`.

- [ ] **Step 5: Commit**

```bash
git add README.md \
        docs/superpowers/specs/2026-04-10-snitchwatch-design.md \
        docs/m0-spike-findings.md
git commit -m "docs: mark M1.5 topology flip done and update README architecture section"
```

---

## Acceptance Criteria

Plan 2 is complete when **all** of the following hold:

1. `cargo check --workspace --all-targets` is clean.
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
3. `cargo test --workspace` is green.
4. `crates/snitchwatch-bridge/src/grpc_client.rs` no longer exists.
5. `crates/snitchwatch-bridge/src/translator/downstream.rs` no longer exists.
6. `tests/bridge_protocol_test.rs` exercises a unary `AskRule` round-trip via the new `MockOpensnitchd::ask_rule` client method.
7. `snitchwatch-bridge-cli` prints both `GRPC_LISTEN_ADDR=...` and `WS_LISTEN_ADDR=...` on startup.
8. `docs/m0-spike-findings.md` has an "Adjustment applied" footer.
9. The README architecture section describes the bridge as the gRPC server, not the gRPC client.
