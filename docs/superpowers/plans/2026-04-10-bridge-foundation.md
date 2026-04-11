# Snitchwatch Bridge Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the headless Snitchwatch bridge — a Rust library that translates between Little Snitch's WebSocket protocol and OpenSnitch's gRPC protocol — plus the M0 spike that proves opensnitchd's `AskRule` semantics actually support the "ask on new connection" UX. Lands as a CLI binary you can wire to either real opensnitchd or an in-process mock, plus a comprehensive test suite.

**Architecture:** Cargo workspace with four crates:
- `snitchwatch-proto` — generated tonic stubs from `vendor/opensnitch/proto/ui.proto`
- `snitchwatch-bridge` — the protocol translator (library), with submodules for translator, cache, ws_server, grpc_client
- `snitchwatch-spike` — the M0 standalone CLI that proves AskRule semantics
- `snitchwatch-bridge-cli` — a thin development CLI that runs the bridge against real or mock daemon

Plus an in-process mock daemon under `tests/mock_opensnitchd/` and an integration test suite that exercises the bridge end-to-end without needing root, eBPF, or a container.

**Tech Stack:** Rust 2021, tokio (async runtime), tonic (gRPC), axum + tokio-tungstenite (WebSocket server), serde / serde_json, tracing (logging), thiserror (error types), regex, proptest (property tests). Daemon container is upstream `opensnitch/opensnitchd:latest` running in podman.

**What this plan does NOT cover** (lives in later plans):
- The vendored `web/` UI (Plan 2)
- Tauri desktop shell, tray, autostart (Plan 3)
- Blocklists subscription manager (Plan 4)
- Docker/headless web variant (Plan 5)
- Flatpak packaging and Bazzite install script (Plan 6)

---

## File structure

```
snitchwatch/
├── .gitignore
├── Cargo.toml                        # workspace root
├── justfile                          # one-liner glue for common tasks
├── README.md                         # placeholder for now
│
├── docs/
│   ├── superpowers/
│   │   ├── specs/2026-04-10-snitchwatch-design.md   # already exists
│   │   └── plans/2026-04-10-bridge-foundation.md    # this file
│   └── m0-spike-findings.md          # written at end of Part A
│
├── vendor/
│   └── opensnitch/                   # git submodule, pinned to release tag
│       └── proto/ui.proto            # consumed by snitchwatch-proto
│
├── crates/
│   ├── snitchwatch-proto/
│   │   ├── Cargo.toml
│   │   ├── build.rs                  # tonic-build invocation
│   │   └── src/lib.rs                # `pub mod ui;`
│   │
│   ├── snitchwatch-spike/
│   │   ├── Cargo.toml
│   │   └── src/main.rs               # M0 standalone CLI
│   │
│   ├── snitchwatch-bridge/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                # `pub mod translator;` etc., re-exports
│   │       ├── error.rs              # BridgeError enum (thiserror)
│   │       ├── ws_server.rs          # axum + WebSocket /stream endpoint
│   │       ├── grpc_client.rs        # tonic client + reconnect loop
│   │       ├── ws_messages.rs        # serde structs for the 22 LS WS messages
│   │       ├── translator/
│   │       │   ├── mod.rs            # re-exports
│   │       │   ├── glob.rs           # glob → regex conversion
│   │       │   ├── specificity.rs    # specificity scoring formula
│   │       │   ├── rule_semantics.rs # LS rule ↔ OpenSnitch rule
│   │       │   ├── downstream.rs     # gRPC events → WS messages
│   │       │   └── upstream.rs       # WS sendAction → gRPC calls
│   │       └── cache/
│   │           ├── mod.rs
│   │           ├── connections.rs    # rolling row buffer + pending oneshots
│   │           └── traffic_bins.rs   # uPlot bucket synthesizer
│   │
│   └── snitchwatch-bridge-cli/
│       ├── Cargo.toml
│       └── src/main.rs               # dev CLI
│
└── tests/
    ├── mock_opensnitchd/
    │   ├── Cargo.toml
    │   └── src/lib.rs                # in-process tonic server
    ├── golden/
    │   └── README.md                 # how golden fixtures work
    └── bridge_protocol_test.rs       # end-to-end integration tests
```

**Why this layout:** every file has one responsibility. The translator subdirectory is split by *what kind of translation*, not by direction — `glob.rs` handles glob→regex regardless of whether it's used downstream or upstream. The cache is its own subdir because it has its own non-trivial state machine. `tests/mock_opensnitchd` is a sibling of the crates because it's only consumed by integration tests, never linked into a release binary.

---

# Part A — M0 spike: prove AskRule semantics

The whole project depends on opensnitchd's `AskRule` actually delivering the UX we want. Before building the bridge, we build a minimal standalone CLI that connects to a real opensnitchd, subscribes to the Notifications stream, prints novel connections, and reads y/n verdicts from stdin. If this works, the bridge approach is sound. If it doesn't, we re-evaluate before sinking weeks into the bridge.

## Task 1: Initialize the workspace

**Files:**
- Create: `Cargo.toml`
- Create: `.gitignore`
- Create: `README.md`
- Create: `justfile`

- [ ] **Step 1: Initialize git repository**

```bash
cd /var/home/user/Documents/vibe-code/opensnitch-gui
git init
git branch -m main
```

- [ ] **Step 2: Write the workspace `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/snitchwatch-proto",
    "crates/snitchwatch-spike",
    "crates/snitchwatch-bridge",
    "crates/snitchwatch-bridge-cli",
    "tests/mock_opensnitchd",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-2.0"
repository = "https://github.com/example/snitchwatch"

[workspace.dependencies]
tokio = { version = "1.40", features = ["full"] }
tonic = "0.12"
tonic-build = "0.12"
prost = "0.13"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
axum = { version = "0.7", features = ["ws"] }
tokio-tungstenite = "0.24"
futures-util = "0.3"
regex = "1"
anyhow = "1"
proptest = "1"
```

- [ ] **Step 3: Write `.gitignore`**

```gitignore
target/
**/*.rs.bk
Cargo.lock.bak
.vscode/
.idea/
*.swp
.DS_Store
```

Note: we deliberately commit `Cargo.lock` since this workspace builds binaries.

- [ ] **Step 4: Write `README.md` placeholder**

```markdown
# Snitchwatch

A Little Snitch–style network firewall GUI for Linux, on top of OpenSnitch.

Status: pre-alpha. See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` for design.
```

- [ ] **Step 5: Write `justfile` skeleton**

```just
default:
    @just --list

build:
    cargo build --workspace

test:
    cargo test --workspace

check:
    cargo check --workspace
    cargo clippy --workspace -- -D warnings

fmt:
    cargo fmt --all
```

- [ ] **Step 6: Verify cargo is happy with an empty workspace**

```bash
mkdir -p crates tests
cargo check --workspace 2>&1 | head -20
```

Expected: errors about missing crate directories — this is fine, we'll fill them in.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml .gitignore README.md justfile
git commit -m "feat: initialize cargo workspace skeleton"
```

## Task 2: Add opensnitch as a submodule

**Files:**
- Create: `vendor/opensnitch/` (submodule)

- [ ] **Step 1: Add the submodule**

```bash
git submodule add https://github.com/evilsocket/opensnitch.git vendor/opensnitch
cd vendor/opensnitch
# pick a recent stable release tag
git fetch --tags
git tag --sort=-v:refname | head -5
```

Expected: a list of recent tags like `v1.6.5`, `v1.6.4`, etc.

- [ ] **Step 2: Pin to the latest stable tag**

```bash
cd vendor/opensnitch
LATEST_TAG=$(git tag --sort=-v:refname | head -1)
git checkout "$LATEST_TAG"
cd ../..
```

- [ ] **Step 3: Verify the proto file exists**

```bash
ls vendor/opensnitch/proto/ui.proto
```

Expected: file exists. If the path is different in your pinned version, find it:

```bash
find vendor/opensnitch -name "ui.proto"
```

Record the actual path; you'll use it in Task 3.

- [ ] **Step 4: Commit**

```bash
git add .gitmodules vendor/opensnitch
git commit -m "feat: vendor opensnitch as submodule pinned to $LATEST_TAG"
```

## Task 3: Build the snitchwatch-proto crate

**Files:**
- Create: `crates/snitchwatch-proto/Cargo.toml`
- Create: `crates/snitchwatch-proto/build.rs`
- Create: `crates/snitchwatch-proto/src/lib.rs`

- [ ] **Step 1: Create the crate structure**

```bash
mkdir -p crates/snitchwatch-proto/src
```

- [ ] **Step 2: Write `crates/snitchwatch-proto/Cargo.toml`**

```toml
[package]
name = "snitchwatch-proto"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
tonic = { workspace = true }
prost = { workspace = true }

[build-dependencies]
tonic-build = { workspace = true }
```

- [ ] **Step 3: Write `crates/snitchwatch-proto/build.rs`**

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path = "../../vendor/opensnitch/proto/ui.proto";
    let proto_dir = "../../vendor/opensnitch/proto";

    println!("cargo:rerun-if-changed={}", proto_path);

    tonic_build::configure()
        .build_server(true)  // we need the server side for mock_opensnitchd
        .build_client(true)
        .compile_protos(&[proto_path], &[proto_dir])?;

    Ok(())
}
```

If your vendored `ui.proto` is at a different path (you found this in Task 2 step 3), use that path instead.

- [ ] **Step 4: Write `crates/snitchwatch-proto/src/lib.rs`**

```rust
//! Generated tonic stubs from opensnitch's ui.proto.
//!
//! The proto file uses the package name `ui` so the generated module is `ui`.
//! If you bump the submodule and the package name changes, update this re-export.

#![allow(clippy::all)]

pub mod ui {
    tonic::include_proto!("ui");
}
```

- [ ] **Step 5: Build it**

```bash
cargo build -p snitchwatch-proto 2>&1 | tail -20
```

Expected: build succeeds. If you see an error like `package "ui" not found`, the proto file uses a different package name. Open `vendor/opensnitch/proto/ui.proto` and look for the `package` declaration:

```bash
grep '^package' vendor/opensnitch/proto/ui.proto
```

Update the `tonic::include_proto!("ui")` line in `lib.rs` to match.

- [ ] **Step 6: Inspect what got generated**

```bash
find target/debug/build -name "ui.rs" | head -1 | xargs head -100
```

Take note of the actual type names: `Connection`, `Notification`, `NotificationReply`, the gRPC service trait, the client struct. These names are what every other crate will reference. **Write them down — you'll need them in Task 4.**

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-proto
git commit -m "feat: add snitchwatch-proto crate with tonic-generated stubs"
```

## Task 4: Build the M0 spike CLI

**Files:**
- Create: `crates/snitchwatch-spike/Cargo.toml`
- Create: `crates/snitchwatch-spike/src/main.rs`

The spike is a standalone binary that connects to a running opensnitchd, dumps the notification stream to stdout, and (when an AskRule arrives) reads y/n from stdin. This is the cheapest possible test of the bridge's central premise.

- [ ] **Step 1: Create the crate structure**

```bash
mkdir -p crates/snitchwatch-spike/src
```

- [ ] **Step 2: Write `crates/snitchwatch-spike/Cargo.toml`**

```toml
[package]
name = "snitchwatch-spike"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
snitchwatch-proto = { path = "../snitchwatch-proto" }
tokio = { workspace = true }
tonic = { workspace = true }
prost = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 3: Write `crates/snitchwatch-spike/src/main.rs`**

This is the meat of M0. The exact gRPC method and message names depend on the proto file you vendored — adjust the `use` statements and method calls to match what tonic-build generated. Run `cargo doc -p snitchwatch-proto --open` after Task 3 if you need to navigate the generated API.

```rust
//! M0 spike — verify that opensnitchd's notification stream and AskRule
//! semantics support the "ask on new connection" UX we want.
//!
//! Usage: cargo run -p snitchwatch-spike -- [grpc_endpoint]
//! Default endpoint: http://127.0.0.1:50051

use anyhow::{Context, Result};
use snitchwatch_proto::ui::ui_client::UiClient;
use std::io::{self, BufRead, Write};
use tokio::sync::mpsc;
use tonic::transport::Channel;
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:50051".to_string());

    info!(%endpoint, "connecting to opensnitchd");

    let channel = Channel::from_shared(endpoint.clone())?
        .connect()
        .await
        .with_context(|| format!("failed to connect to {}", endpoint))?;

    let mut client = UiClient::new(channel);

    // The exact RPC method here depends on opensnitch's ui.proto.
    // Most versions expose a server-streaming `Notifications` RPC that takes
    // a `NotificationReply` stream from the client and yields `Notification`
    // events from the server. Adjust the request type to match what
    // `cargo doc -p snitchwatch-proto` shows.
    //
    // The bidirectional pattern is:
    //   client -> server: NotificationReply (verdict for prior AskRule)
    //   server -> client: Notification (event, optionally an AskRule)

    let (tx, rx) = mpsc::channel(32);
    let outbound = tokio_stream::wrappers::ReceiverStream::new(rx);

    // The first message sent is usually a "hello"-style empty reply that
    // identifies the client. Verify against the proto.
    let initial = snitchwatch_proto::ui::NotificationReply {
        id: 0,
        code: 0,
        data: String::new(),
    };
    tx.send(initial).await.ok();

    let response = client.notifications(outbound).await?;
    let mut inbound = response.into_inner();

    info!("subscribed to notification stream — waiting for events");

    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();

    while let Some(notification) = inbound.message().await? {
        info!(?notification, "received notification");

        // The notification carries a `type` enum and possibly a connection
        // payload. AskRule notifications block the kernel packet until we
        // reply. Replies are sent back through the same `tx` channel using
        // the notification's `id` field as correlation.
        //
        // Pseudocode (refine to match actual proto types):
        //
        // match notification.r#type {
        //     NotificationType::AskRule => {
        //         let conn = notification.data; // a serialized Connection
        //         println!("AskRule from {}: {}", conn.process, conn.dst_host);
        //         print!("allow? [y/N] ");
        //         io::stdout().flush()?;
        //         let mut line = String::new();
        //         stdin_lock.read_line(&mut line)?;
        //         let action = if line.trim().eq_ignore_ascii_case("y") {
        //             "allow"
        //         } else {
        //             "deny"
        //         };
        //         let reply = NotificationReply {
        //             id: notification.id,
        //             code: 0,
        //             data: action.to_string(),
        //         };
        //         tx.send(reply).await?;
        //     }
        //     _ => { /* log and ignore */ }
        // }
        //
        // For the spike, just print every notification verbatim. We'll
        // refine the verdict path in Task 5 once we see what the daemon
        // actually sends.

        let _ = stdin_lock; // silence unused warning until we wire stdin
    }

    warn!("notification stream ended");
    Ok(())
}
```

> **NOTE for the implementer:** the exact field names (`r#type`, `id`, `code`, `data`) and the exact RPC method (`notifications` vs `Subscribe` vs `Stream`) depend on the proto version you vendored. After you generate the stubs in Task 3, run `cargo doc -p snitchwatch-proto --open` and navigate to the `ui` module to see what tonic actually built. Adjust the spike code to match. **This is the spike's job — discover the actual API surface.**

- [ ] **Step 4: Add tokio-stream to the spike's dependencies**

Edit `crates/snitchwatch-spike/Cargo.toml` and add:

```toml
tokio-stream = "0.1"
```

- [ ] **Step 5: Build the spike**

```bash
cargo build -p snitchwatch-spike 2>&1 | tail -30
```

Expected: most likely some compilation errors because the field names don't match the actual proto. **This is the spike teaching you the API.** Read the errors, fix the references in `main.rs`, repeat. Keep going until it builds clean.

- [ ] **Step 6: Commit the (still rough) spike**

```bash
git add crates/snitchwatch-spike
git commit -m "feat: add M0 spike CLI scaffolding"
```

## Task 5: Run the spike against a real opensnitchd

**Files:**
- Create: `docs/m0-spike-findings.md`

This task is investigative — its output is documentation, not code.

- [ ] **Step 1: Run opensnitchd in podman**

```bash
podman run -d --rm \
    --name opensnitchd-spike \
    --privileged \
    --network=host \
    --pid=host \
    --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
    -v /var/log/opensnitch:/var/log/opensnitch:Z \
    docker.io/evilsocket/opensnitch:latest
```

Expected: container starts. Verify with:

```bash
podman ps
podman logs opensnitchd-spike 2>&1 | tail -20
```

If the official image isn't available, build it from `vendor/opensnitch/Dockerfile` (path may vary):

```bash
find vendor/opensnitch -name "Dockerfile*"
```

- [ ] **Step 2: Verify the daemon is listening on 50051**

```bash
ss -ltnp | grep 50051
```

Expected: a line showing something is listening on 127.0.0.1:50051 or 0.0.0.0:50051.

- [ ] **Step 3: Run the spike**

```bash
RUST_LOG=info cargo run -p snitchwatch-spike
```

- [ ] **Step 4: Trigger a novel connection**

In another terminal:

```bash
curl -s https://example.invalid >/dev/null 2>&1 || true
curl -s https://httpbin.org/get >/dev/null
```

Watch the spike output. If everything works, you should see notification events fly past.

- [ ] **Step 5: Document findings in `docs/m0-spike-findings.md`**

Write what you actually observed. Include:

```markdown
# M0 Spike Findings — opensnitchd notification semantics

**Spike date:** YYYY-MM-DD
**opensnitchd version:** (from `podman exec opensnitchd-spike opensnitchd --version`)
**ui.proto submodule pin:** vX.Y.Z (the tag you pinned in Task 2)

## Verified facts

- [ ] The `Notifications` RPC is bidirectional: `stream NotificationReply -> stream Notification` (or correct it to whatever you saw)
- [ ] The first message we send is an "identify" reply with id=0
- [ ] AskRule events arrive with `type = ASK_RULE` (or whatever the enum value is named)
- [ ] AskRule events block the kernel packet until we send a reply with the matching `id`
- [ ] The `data` field in an AskRule contains a serialized Connection (JSON / protobuf-encoded / etc — record what you saw)
- [ ] Reply format for a verdict: `{id, code, data}` where `data` is `"allow"` / `"deny"` / etc — record the actual schema

## Config keys verified

For each of these the spec assumed exists, check what's actually in opensnitchd's `default-config.json` or equivalent:

- [ ] `default_action` — confirmed exists, takes values: ___
- [ ] `intercept_unknown` — confirmed exists, boolean
- [ ] AskRule timeout — confirmed exists at config key `___`, default value ___

## Surprises

(Anything different from what the spec assumed.)

## Recommendation

- [ ] PROCEED — semantics match the design, bridge implementation can begin
- [ ] PROCEED WITH ADJUSTMENT — the design needs tweaks; document them here
- [ ] HALT — fundamental mismatch, re-evaluate the project

Detailed adjustments needed for the spec (if any):
```

- [ ] **Step 6: Stop opensnitchd**

```bash
podman stop opensnitchd-spike
```

- [ ] **Step 7: Commit findings**

```bash
git add docs/m0-spike-findings.md crates/snitchwatch-spike
git commit -m "docs: M0 spike findings — opensnitchd AskRule semantics verified"
```

> **GATE:** Do not proceed past Part A unless the spike findings document says PROCEED or PROCEED WITH ADJUSTMENT. If it says HALT, stop and reconsider the design before any more code is written.

---

# Part B — Bridge crate skeleton

Now we build the bridge as a library crate, with the public surface but mostly empty modules. We fill them in over Parts C through F.

## Task 6: Create the bridge crate skeleton

**Files:**
- Create: `crates/snitchwatch-bridge/Cargo.toml`
- Create: `crates/snitchwatch-bridge/src/lib.rs`
- Create: `crates/snitchwatch-bridge/src/error.rs`

- [ ] **Step 1: Create the directory structure**

```bash
mkdir -p crates/snitchwatch-bridge/src/translator
mkdir -p crates/snitchwatch-bridge/src/cache
```

- [ ] **Step 2: Write `crates/snitchwatch-bridge/Cargo.toml`**

```toml
[package]
name = "snitchwatch-bridge"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
snitchwatch-proto = { path = "../snitchwatch-proto" }
tokio = { workspace = true }
tonic = { workspace = true }
prost = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
axum = { workspace = true }
tokio-tungstenite = { workspace = true }
futures-util = { workspace = true }
regex = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 3: Write `crates/snitchwatch-bridge/src/lib.rs`**

```rust
//! Snitchwatch bridge — translates between Little Snitch's WebSocket protocol
//! and OpenSnitch's gRPC protocol.
//!
//! This crate is intentionally headless. It can be exercised against either a
//! real opensnitchd (via the gRPC client) or `tests/mock_opensnitchd` (an
//! in-process tonic server). It does not depend on Tauri, WebKitGTK, or any
//! windowing system.

pub mod cache;
pub mod error;
pub mod grpc_client;
pub mod translator;
pub mod ws_messages;
pub mod ws_server;

pub use error::BridgeError;
```

- [ ] **Step 4: Write `crates/snitchwatch-bridge/src/error.rs`**

```rust
//! Error types for the bridge.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("rule translation failed: {reason}")]
    RuleTranslation { reason: String },

    #[error("connection cache error: {reason}")]
    Cache { reason: String },

    #[error("daemon disconnected — reconnecting")]
    Disconnected,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 5: Write empty module stubs so the crate compiles**

Create each of these as a one-line file. We'll fill them in over the next several tasks.

`crates/snitchwatch-bridge/src/grpc_client.rs`:
```rust
//! gRPC client to opensnitchd. See Task 18.
```

`crates/snitchwatch-bridge/src/ws_server.rs`:
```rust
//! WebSocket server for the embedded webview. See Task 19.
```

`crates/snitchwatch-bridge/src/ws_messages.rs`:
```rust
//! Serde structs for the 22 LS WebSocket message types. See Task 9.
```

`crates/snitchwatch-bridge/src/translator/mod.rs`:
```rust
//! LS ↔ OpenSnitch protocol translation.

pub mod downstream;
pub mod glob;
pub mod rule_semantics;
pub mod specificity;
pub mod upstream;
```

`crates/snitchwatch-bridge/src/translator/glob.rs`:
```rust
//! Glob → regex conversion. See Task 11.
```

`crates/snitchwatch-bridge/src/translator/specificity.rs`:
```rust
//! Rule specificity scoring. See Task 12.
```

`crates/snitchwatch-bridge/src/translator/rule_semantics.rs`:
```rust
//! LS rule ↔ OpenSnitch rule. See Task 13.
```

`crates/snitchwatch-bridge/src/translator/downstream.rs`:
```rust
//! gRPC events → WS messages. See Task 16.
```

`crates/snitchwatch-bridge/src/translator/upstream.rs`:
```rust
//! WS sendAction → gRPC calls. See Task 17.
```

`crates/snitchwatch-bridge/src/cache/mod.rs`:
```rust
//! In-memory presentation cache (connections + traffic chart).

pub mod connections;
pub mod traffic_bins;
```

`crates/snitchwatch-bridge/src/cache/connections.rs`:
```rust
//! Rolling connection-row buffer with pending-prompt machinery. See Task 14.
```

`crates/snitchwatch-bridge/src/cache/traffic_bins.rs`:
```rust
//! uPlot bucket synthesizer. See Task 15.
```

- [ ] **Step 6: Verify the crate builds**

```bash
cargo check -p snitchwatch-bridge 2>&1 | tail -20
```

Expected: builds clean (warnings about unused imports are fine).

- [ ] **Step 7: Commit**

```bash
git add crates/snitchwatch-bridge
git commit -m "feat: scaffold snitchwatch-bridge crate with empty modules"
```

---

# Part C — Pure functions (translator + ws messages)

These tasks build the parts of the bridge that have no I/O — just data transformations. They're easy to test exhaustively and they're the foundation everything else stands on.

## Task 7: Define the LS WebSocket message types

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_messages.rs`
- Test: `crates/snitchwatch-bridge/src/ws_messages.rs` (inline `#[cfg(test)]` module)

The 22 message types are listed in the spec. We model them as a tagged enum so serde can dispatch on the `action` field — the same way the LS UI's `handleServerCommand` does.

- [ ] **Step 1: Write the failing test first**

Add to `crates/snitchwatch-bridge/src/ws_messages.rs`:

```rust
//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.

use serde::{Deserialize, Serialize};

/// Server → client message envelope. Each variant matches one of the 22
/// `handleServerCommand` cases in the LS UI's `app.js`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ServerMessage {
    InsertConnectionRows { rows: Vec<ConnectionRow> },
    UpdateConnectionRows { rows: Vec<ConnectionRow> },
    RemoveConnectionRows { ids: Vec<String> },
    /// Note the typo: this is in upstream LS, we preserve it.
    #[serde(rename = "moveConnetionRows")]
    MoveConnetionRows { ids: Vec<String> },
    ClearConnectionRows,
    SetInspector { inspector: serde_json::Value },
    UpdateRuleButtons { buttons: serde_json::Value },
    HighlightRuleForRows { rule_id: String, row_ids: Vec<String> },
    TrafficEvents { events: Vec<TrafficEvent> },
    SetTrafficData { data: serde_json::Value },
    UpdateTrafficData { data: serde_json::Value },
    SetRules { rules: Vec<serde_json::Value> },
    UpdateRules { rules: Vec<serde_json::Value> },
    SetBlocklists { blocklists: Vec<serde_json::Value> },
    SetBlocklistDetails { details: serde_json::Value },
    SetBlocklistEntries { entries: Vec<serde_json::Value> },
    SetBlocklistEntryLocation { location: serde_json::Value },
    SetBlocklistStatus { status: serde_json::Value },
    SetConnectionsStatus { status: ConnectionsStatus },
    SetAboutInfo { info: AboutInfo },
    SetUndoStack { stack: Vec<serde_json::Value> },
    LocalizationTable { table: serde_json::Value },
    GlobalSettings { settings: serde_json::Value },
}

/// Client → server messages. These come from the UI's `sendAction(type, payload)`
/// calls. The `action` discriminator is the type name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ClientMessage {
    SetVerdict {
        row_id: String,
        action: VerdictAction,
        scope: VerdictScope,
        remember: bool,
    },
    AddRule { rule: serde_json::Value },
    UpdateRule { rule_id: String, rule: serde_json::Value },
    DeleteRule { rule_id: String },
    GlobalSettings { settings: serde_json::Value },
    SubscribeBlocklist { url: String },
    UnsubscribeBlocklist { id: String },
    Undo,
    Redo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VerdictAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictScope {
    /// Exact destination host only.
    ThisHost,
    /// Wildcard the leftmost label of the destination host.
    AnyHostOnDomain,
    /// Drop the host operator entirely.
    AnyHost,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRow {
    pub id: String,
    pub process: String,
    pub process_path: Option<String>,
    pub dst_host: String,
    pub dst_ip: String,
    pub dst_port: u16,
    pub protocol: String,
    pub direction: String,
    /// `null` for pending rows, `"allow"` / `"deny"` / `"blocklist"` once decided.
    pub action: Option<String>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrafficEvent {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionsStatus {
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfo {
    pub snitchwatch_version: String,
    pub opensnitchd_version: String,
    pub ebpf_commit: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_round_trips_via_json() {
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![ConnectionRow {
                id: "r1".to_string(),
                process: "firefox".to_string(),
                process_path: Some("/usr/bin/firefox".to_string()),
                dst_host: "github.com".to_string(),
                dst_ip: "140.82.121.4".to_string(),
                dst_port: 443,
                protocol: "tcp".to_string(),
                direction: "outgoing".to_string(),
                action: None,
                bytes_sent: 0,
                bytes_received: 0,
                started_at_ms: 1_700_000_000_000,
            }],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""action":"insertConnectionRows""#));
        assert!(json.contains(r#""dstHost":"github.com""#));

        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn move_connection_rows_preserves_upstream_typo() {
        let msg = ServerMessage::MoveConnetionRows {
            ids: vec!["r1".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains(r#""action":"moveConnetionRows""#),
            "must preserve upstream LS typo: {}",
            json
        );
    }

    #[test]
    fn client_set_verdict_parses() {
        let json = r#"{
            "action": "setVerdict",
            "rowId": "r1",
            "actionField": "ignored",
            "action": "allow",
            "scope": "this_host",
            "remember": true
        }"#;
        // Note: `action` collides with the discriminator. The real LS UI uses
        // a different field name; we'll align once we capture a real payload.
        // For now we test the simpler shape:
        let json = r#"{
            "action": "setVerdict",
            "rowId": "r1",
            "scope": "this_host",
            "remember": true
        }"#;
        // This will fail until we resolve the action-field collision.
        // (See Task 7 step 4 for the fix.)
        let result: Result<ClientMessage, _> = serde_json::from_str(json);
        assert!(result.is_err(), "we expect this to need an explicit verdict field");
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p snitchwatch-bridge ws_messages 2>&1 | tail -20
```

Expected: tests run, the verdict test passes (because we expect failure), the round-trip tests pass.

- [ ] **Step 3: Resolve the verdict-action field collision**

The LS protocol uses `action` as both the message discriminator AND the verdict value inside `setVerdict`. We disambiguate by renaming the inner field. Since the real LS UI's `sendAction` builds `{action, ...payload}`, the inner verdict in their payload is named differently — capture an actual `setVerdict` message from the live LS instance to confirm. For now, assume the inner field is `verdict` and document the assumption:

Update the `ClientMessage::SetVerdict` variant:

```rust
SetVerdict {
    row_id: String,
    /// "allow" or "deny" — renamed from `action` to avoid colliding with the
    /// envelope's `action` discriminator. Verify against captured LS payload.
    #[serde(rename = "verdict")]
    verdict: VerdictAction,
    scope: VerdictScope,
    remember: bool,
},
```

And update the failing test to assert the new shape parses:

```rust
#[test]
fn client_set_verdict_parses() {
    let json = r#"{
        "action": "setVerdict",
        "rowId": "r1",
        "verdict": "allow",
        "scope": "this_host",
        "remember": true
    }"#;
    let parsed: ClientMessage = serde_json::from_str(json).unwrap();
    match parsed {
        ClientMessage::SetVerdict { row_id, verdict, scope, remember } => {
            assert_eq!(row_id, "r1");
            assert_eq!(verdict, VerdictAction::Allow);
            assert_eq!(scope, VerdictScope::ThisHost);
            assert!(remember);
        }
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 4: Run tests again**

```bash
cargo test -p snitchwatch-bridge ws_messages 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_messages.rs
git commit -m "feat(bridge): define LS WebSocket message types with serde"
```

> **Followup task to track:** capture real `setVerdict` payloads from the local LS instance at `http://localhost:3031/` (use the browser devtools network tab → WS → messages) and reconcile field names. File this as an issue or a TODO comment in `ws_messages.rs`.

## Task 8: Glob → regex conversion

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/glob.rs`

LS rules let users write `*.github.com` style globs for the host operand. OpenSnitch needs a regex. The bridge does the conversion.

- [ ] **Step 1: Write the failing test first**

```rust
//! Glob → regex conversion for host operands.
//!
//! Supports the LS-style subset:
//!   * `*` matches any sequence of non-`.` characters within one label
//!   * `**` matches any sequence including `.`
//!   * literal `.` is escaped to `\.`
//!   * the result is anchored with `^` and `$`

use regex::Regex;

#[derive(Debug, thiserror::Error)]
pub enum GlobError {
    #[error("invalid regex produced: {0}")]
    InvalidRegex(#[from] regex::Error),
}

/// Convert an LS-style glob to an anchored regex string.
pub fn glob_to_regex_string(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() * 2 + 2);
    out.push('^');

    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    out.push_str(".*");
                } else {
                    out.push_str("[^.]*");
                }
            }
            '.' => out.push_str("\\."),
            '?' => out.push_str("[^.]"),
            // Escape regex metacharacters
            '+' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }

    out.push('$');
    out
}

/// Convert and compile.
pub fn glob_to_regex(glob: &str) -> Result<Regex, GlobError> {
    Ok(Regex::new(&glob_to_regex_string(glob))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_host_round_trips() {
        assert_eq!(glob_to_regex_string("github.com"), r"^github\.com$");
        let re = glob_to_regex("github.com").unwrap();
        assert!(re.is_match("github.com"));
        assert!(!re.is_match("api.github.com"));
        assert!(!re.is_match("github.com.evil.com"));
    }

    #[test]
    fn star_matches_one_label() {
        let re = glob_to_regex("*.github.com").unwrap();
        assert!(re.is_match("api.github.com"));
        assert!(re.is_match("raw.github.com"));
        assert!(!re.is_match("github.com"), "no label means no match");
        assert!(!re.is_match("api.cdn.github.com"), "two labels need **");
    }

    #[test]
    fn double_star_matches_multiple_labels() {
        let re = glob_to_regex("**.github.com").unwrap();
        assert!(re.is_match("api.github.com"));
        assert!(re.is_match("api.cdn.github.com"));
        assert!(re.is_match(".github.com")); // edge case: empty subdomain
    }

    #[test]
    fn dots_are_escaped() {
        let re = glob_to_regex("a.b.c").unwrap();
        assert!(re.is_match("a.b.c"));
        assert!(!re.is_match("aXbXc"), "dots must be literal, not regex .");
    }

    #[test]
    fn metacharacters_are_escaped() {
        let re = glob_to_regex("a+b").unwrap();
        assert!(re.is_match("a+b"));
        assert!(!re.is_match("ab"));
    }

    #[test]
    fn question_mark_matches_one_non_dot_char() {
        let re = glob_to_regex("a?c.com").unwrap();
        assert!(re.is_match("abc.com"));
        assert!(!re.is_match("ac.com"));
        assert!(!re.is_match("a.c.com"));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p snitchwatch-bridge translator::glob 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/glob.rs
git commit -m "feat(bridge): glob → regex conversion for host operands"
```

## Task 9: Specificity scoring

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/specificity.rs`

OpenSnitch evaluates rules in alphabetical order by filename and stops at the first match. LS implicitly uses "most specific wins". The bridge bridges these by computing a specificity score and prefixing the rule name with `{999 - score:03d}-`.

- [ ] **Step 1: Write the test first**

```rust
//! Rule specificity scoring.
//!
//! LS implicitly resolves rule conflicts by "most specific wins". OpenSnitch
//! evaluates rules in alphabetical order by filename. The bridge translates
//! LS specificity into explicit alphabetical prefixes.
//!
//! Score formula:
//!   100 * has(process)
//! +  50 * has(remote_host_exact)
//! +  30 * has(remote_host_glob)
//! +  40 * has(port)
//! +  20 * has(protocol)
//!
//! Filename = f"{999 - score:03d}-{slug}.json"
//! Higher specificity → lower number → evaluated first.
//!
//! Blocklists are clamped to score = 0 (filename prefix 900–999) so user
//! rules always win — see `BLOCKLIST_BAND_BASE`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecificityInputs {
    pub has_process: bool,
    pub has_remote_host_exact: bool,
    pub has_remote_host_glob: bool,
    pub has_port: bool,
    pub has_protocol: bool,
}

pub const SCORE_PROCESS: u32 = 100;
pub const SCORE_HOST_EXACT: u32 = 50;
pub const SCORE_HOST_GLOB: u32 = 30;
pub const SCORE_PORT: u32 = 40;
pub const SCORE_PROTOCOL: u32 = 20;
pub const SCORE_MAX: u32 = SCORE_PROCESS + SCORE_HOST_EXACT + SCORE_PORT + SCORE_PROTOCOL; // 210

/// Filename prefix range reserved for blocklist rules. Always *worse* than
/// any user rule because user rules use the 0–999 range.
pub const BLOCKLIST_BAND_BASE: u32 = 900;

pub fn score(inputs: &SpecificityInputs) -> u32 {
    let mut s = 0;
    if inputs.has_process {
        s += SCORE_PROCESS;
    }
    if inputs.has_remote_host_exact {
        s += SCORE_HOST_EXACT;
    } else if inputs.has_remote_host_glob {
        s += SCORE_HOST_GLOB;
    }
    if inputs.has_port {
        s += SCORE_PORT;
    }
    if inputs.has_protocol {
        s += SCORE_PROTOCOL;
    }
    s
}

/// Compute a filename prefix for a user rule. Lower prefix = higher priority.
pub fn user_rule_prefix(inputs: &SpecificityInputs) -> String {
    let s = score(inputs).min(999);
    format!("{:03}", 999u32.saturating_sub(s))
}

/// Compute a filename prefix for a blocklist rule. Always in the 900–999 band.
/// `entry_index` lets multiple blocklist rules order deterministically within
/// the band.
pub fn blocklist_rule_prefix(entry_index: u32) -> String {
    // Cap at 999 so we never escape the band even with 100k+ entries.
    let offset = entry_index.min(99);
    format!("{}", BLOCKLIST_BAND_BASE + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(p: bool, he: bool, hg: bool, port: bool, proto: bool) -> SpecificityInputs {
        SpecificityInputs {
            has_process: p,
            has_remote_host_exact: he,
            has_remote_host_glob: hg,
            has_port: port,
            has_protocol: proto,
        }
    }

    #[test]
    fn empty_rule_scores_zero() {
        assert_eq!(score(&inputs(false, false, false, false, false)), 0);
    }

    #[test]
    fn process_only_scores_100() {
        assert_eq!(score(&inputs(true, false, false, false, false)), 100);
    }

    #[test]
    fn process_plus_exact_host_plus_port_plus_proto() {
        // 100 + 50 + 40 + 20 = 210
        assert_eq!(score(&inputs(true, true, false, true, true)), 210);
    }

    #[test]
    fn exact_host_beats_glob_host() {
        let exact = score(&inputs(true, true, false, true, true));
        let glob = score(&inputs(true, false, true, true, true));
        assert!(exact > glob, "exact should beat glob");
    }

    #[test]
    fn exact_and_glob_set_uses_exact_only() {
        // If both flags are accidentally set, exact wins (we don't double-count).
        let s = score(&inputs(false, true, true, false, false));
        assert_eq!(s, SCORE_HOST_EXACT);
    }

    #[test]
    fn user_prefix_more_specific_sorts_first() {
        let specific = user_rule_prefix(&inputs(true, true, false, true, true));
        let generic = user_rule_prefix(&inputs(true, false, false, false, false));
        assert!(specific < generic, "{} should sort before {}", specific, generic);
    }

    #[test]
    fn blocklist_prefix_in_900_band() {
        let p = blocklist_rule_prefix(42);
        assert!(p.starts_with("9"), "blocklist prefix should be 9xx, got {}", p);
        // Even rule 0 in a blocklist beats every user rule
        assert!(blocklist_rule_prefix(0) > user_rule_prefix(&inputs(false, false, false, false, false)));
    }

    #[test]
    fn user_prefix_zero_score_is_999() {
        assert_eq!(user_rule_prefix(&inputs(false, false, false, false, false)), "999");
    }

    #[test]
    fn user_prefix_max_score_is_smallest() {
        let p = user_rule_prefix(&inputs(true, true, false, true, true));
        // 999 - 210 = 789
        assert_eq!(p, "789");
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p snitchwatch-bridge translator::specificity 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/specificity.rs
git commit -m "feat(bridge): rule specificity scoring formula"
```

## Task 10: Rule semantics translator (LS rule ↔ OpenSnitch rule)

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/rule_semantics.rs`

This is the largest pure-function task. We define the LS rule shape and the OpenSnitch rule shape (the opensnitch rule shape lives in the proto-generated types but we wrap them in a friendlier domain type), and we implement bidirectional translation.

- [ ] **Step 1: Define the domain types**

```rust
//! Bidirectional LS rule ↔ OpenSnitch rule translation.

use crate::translator::{glob, specificity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsRule {
    pub id: String,
    pub name: String,
    pub verdict: LsVerdict,
    pub direction: LsDirection,
    pub scope: LsScope,
    pub permanent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LsVerdict {
    Allow,
    Deny,
    Blocklist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LsDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsScope {
    pub process: Option<String>,
    pub remote_host: Option<RemoteHost>,
    pub port: Option<u16>,
    pub protocol: Option<Protocol>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteHost {
    Exact(String),
    Glob(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    Any,
}

/// A friendlier wrapper around the proto-generated opensnitchd Rule type.
/// We don't use the proto type directly because (a) we want to test
/// translation without depending on tonic-build output and (b) we control
/// the field shape this way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsRule {
    pub name: String,
    pub enabled: bool,
    pub action: OsAction,
    pub duration: OsDuration,
    pub operator: OsOperator,
    /// `__source: "user"` or `"blocklist:<id>"` or `"unenforced:incoming"`.
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsAction {
    Allow,
    Deny,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsDuration {
    Once,
    UntilRestart,
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OsOperator {
    Simple { operand: OsOperand, data: String },
    Regexp { operand: OsOperand, data: String },
    List { operands: Vec<OsOperator> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsOperand {
    ProcessPath,
    DestHost,
    DestPort,
    Protocol,
    Direction,
}

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("unsupported rule shape: {0}")]
    Unsupported(String),
}
```

- [ ] **Step 2: Run cargo check to verify the types compile**

```bash
cargo check -p snitchwatch-bridge 2>&1 | tail -20
```

Expected: clean compile.

- [ ] **Step 3: Commit the type definitions**

```bash
git add crates/snitchwatch-bridge/src/translator/rule_semantics.rs
git commit -m "feat(bridge): rule semantics domain types"
```

- [ ] **Step 4: Write the failing translation test**

Append to `rule_semantics.rs`:

```rust
/// Translate an LsRule into one or more OpenSnitch rules. Returns multiple
/// when `direction = Both` (one per direction).
pub fn ls_to_os(ls: &LsRule) -> Result<Vec<OsRule>, TranslateError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_only_allow() -> LsRule {
        LsRule {
            id: "r1".to_string(),
            name: "firefox-allow".to_string(),
            verdict: LsVerdict::Allow,
            direction: LsDirection::Outgoing,
            scope: LsScope {
                process: Some("/usr/bin/firefox".to_string()),
                remote_host: None,
                port: None,
                protocol: None,
            },
            permanent: true,
        }
    }

    #[test]
    fn process_only_allow_translates_to_single_os_rule() {
        let ls = process_only_allow();
        let out = ls_to_os(&ls).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].action, OsAction::Allow);
        assert_eq!(out[0].duration, OsDuration::Always);
        assert!(out[0].name.starts_with("899-"), "process-only score=100, prefix=899: {}", out[0].name);
        match &out[0].operator {
            OsOperator::Simple { operand, data } => {
                assert_eq!(*operand, OsOperand::ProcessPath);
                assert_eq!(data, "/usr/bin/firefox");
            }
            other => panic!("expected Simple, got {:?}", other),
        }
    }
}
```

- [ ] **Step 5: Run the test, expect failure**

```bash
cargo test -p snitchwatch-bridge translator::rule_semantics 2>&1 | tail -20
```

Expected: `not yet implemented` panic from `todo!()`.

- [ ] **Step 6: Implement `ls_to_os`**

Replace the `todo!()` body with:

```rust
pub fn ls_to_os(ls: &LsRule) -> Result<Vec<OsRule>, TranslateError> {
    let directions: Vec<LsDirection> = match ls.direction {
        LsDirection::Outgoing => vec![LsDirection::Outgoing],
        LsDirection::Incoming => vec![LsDirection::Incoming],
        LsDirection::Both => vec![LsDirection::Outgoing, LsDirection::Incoming],
    };

    let inputs = specificity::SpecificityInputs {
        has_process: ls.scope.process.is_some(),
        has_remote_host_exact: matches!(ls.scope.remote_host, Some(RemoteHost::Exact(_))),
        has_remote_host_glob: matches!(ls.scope.remote_host, Some(RemoteHost::Glob(_))),
        has_port: ls.scope.port.is_some(),
        has_protocol: !matches!(ls.scope.protocol, None | Some(Protocol::Any)),
    };
    let prefix = specificity::user_rule_prefix(&inputs);

    let action = match ls.verdict {
        LsVerdict::Allow => OsAction::Allow,
        LsVerdict::Deny | LsVerdict::Blocklist => OsAction::Deny,
    };

    let duration = if ls.permanent {
        OsDuration::Always
    } else {
        OsDuration::UntilRestart
    };

    let mut out = Vec::with_capacity(directions.len());

    for dir in directions {
        let mut operators: Vec<OsOperator> = Vec::new();

        if let Some(process) = &ls.scope.process {
            operators.push(OsOperator::Simple {
                operand: OsOperand::ProcessPath,
                data: process.clone(),
            });
        }

        if let Some(host) = &ls.scope.remote_host {
            match host {
                RemoteHost::Exact(h) => operators.push(OsOperator::Simple {
                    operand: OsOperand::DestHost,
                    data: h.clone(),
                }),
                RemoteHost::Glob(g) => operators.push(OsOperator::Regexp {
                    operand: OsOperand::DestHost,
                    data: glob::glob_to_regex_string(g),
                }),
            }
        }

        if let Some(port) = ls.scope.port {
            operators.push(OsOperator::Simple {
                operand: OsOperand::DestPort,
                data: port.to_string(),
            });
        }

        if let Some(proto) = ls.scope.protocol {
            if !matches!(proto, Protocol::Any) {
                operators.push(OsOperator::Simple {
                    operand: OsOperand::Protocol,
                    data: match proto {
                        Protocol::Tcp => "tcp".to_string(),
                        Protocol::Udp => "udp".to_string(),
                        Protocol::Any => unreachable!(),
                    },
                });
            }
        }

        // Always add the direction operator so the daemon knows which chain
        // to evaluate this rule in.
        operators.push(OsOperator::Simple {
            operand: OsOperand::Direction,
            data: match dir {
                LsDirection::Outgoing => "outgoing".to_string(),
                LsDirection::Incoming => "incoming".to_string(),
                LsDirection::Both => unreachable!(),
            },
        });

        let operator = if operators.len() == 1 {
            operators.into_iter().next().unwrap()
        } else {
            OsOperator::List { operands: operators }
        };

        // Mark incoming-direction rules as unenforced because v1 of the
        // bridge does not enable opensnitchd's incoming hook by default.
        let description = if matches!(dir, LsDirection::Incoming) {
            format!(r#"{{"snitchwatch":{{"source":"user","ls_rule_id":"{}","status":"unenforced"}}}}"#, ls.id)
        } else {
            format!(r#"{{"snitchwatch":{{"source":"user","ls_rule_id":"{}","status":"enforced"}}}}"#, ls.id)
        };

        let dir_suffix = match dir {
            LsDirection::Outgoing => "out",
            LsDirection::Incoming => "in",
            LsDirection::Both => unreachable!(),
        };

        let name = format!("{}-{}-{}.json", prefix, slugify(&ls.name), dir_suffix);

        out.push(OsRule {
            name,
            enabled: true,
            action,
            duration,
            operator,
            description,
        });
    }

    Ok(out)
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}
```

- [ ] **Step 7: Run the test**

```bash
cargo test -p snitchwatch-bridge translator::rule_semantics 2>&1 | tail -20
```

Expected: `process_only_allow_translates_to_single_os_rule` passes. Note the test expects the operator to be a single `Simple` but our new code wraps a process+direction pair in a `List`. **The test needs an update OR the implementation needs to skip the direction operator when it's the only thing besides the user-supplied operators.**

We choose: keep the direction operator unconditional (the daemon needs it) and update the test to expect a `List`:

```rust
#[test]
fn process_only_allow_translates_to_single_os_rule() {
    let ls = process_only_allow();
    let out = ls_to_os(&ls).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].action, OsAction::Allow);
    assert_eq!(out[0].duration, OsDuration::Always);
    assert!(out[0].name.starts_with("899-"), "process-only score=100, prefix=899: {}", out[0].name);
    match &out[0].operator {
        OsOperator::List { operands } => {
            assert!(operands.iter().any(|op| matches!(
                op,
                OsOperator::Simple { operand: OsOperand::ProcessPath, data } if data == "/usr/bin/firefox"
            )));
            assert!(operands.iter().any(|op| matches!(
                op,
                OsOperator::Simple { operand: OsOperand::Direction, data } if data == "outgoing"
            )));
        }
        other => panic!("expected List, got {:?}", other),
    }
}
```

- [ ] **Step 8: Run the test again**

```bash
cargo test -p snitchwatch-bridge translator::rule_semantics 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 9: Add tests for the rest of the mapping table**

Append to the `tests` module:

```rust
fn deny_with_glob_host() -> LsRule {
    LsRule {
        id: "r2".to_string(),
        name: "block-tracker".to_string(),
        verdict: LsVerdict::Deny,
        direction: LsDirection::Outgoing,
        scope: LsScope {
            process: None,
            remote_host: Some(RemoteHost::Glob("*.tracker.example".to_string())),
            port: None,
            protocol: None,
        },
        permanent: true,
    }
}

#[test]
fn glob_host_becomes_regexp_operator() {
    let out = ls_to_os(&deny_with_glob_host()).unwrap();
    assert_eq!(out.len(), 1);
    let host_op = match &out[0].operator {
        OsOperator::List { operands } => operands.iter().find(|o| matches!(
            o,
            OsOperator::Regexp { operand: OsOperand::DestHost, .. }
        )).expect("must contain a Regexp host operand"),
        _ => panic!("expected List"),
    };
    if let OsOperator::Regexp { data, .. } = host_op {
        assert_eq!(data, r"^[^.]*\.tracker\.example$");
    }
}

#[test]
fn both_direction_yields_two_rules() {
    let mut ls = process_only_allow();
    ls.direction = LsDirection::Both;
    let out = ls_to_os(&ls).unwrap();
    assert_eq!(out.len(), 2);
    let names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
    assert!(names.iter().any(|n| n.ends_with("-out.json")));
    assert!(names.iter().any(|n| n.ends_with("-in.json")));
}

#[test]
fn incoming_rule_is_marked_unenforced() {
    let mut ls = process_only_allow();
    ls.direction = LsDirection::Incoming;
    let out = ls_to_os(&ls).unwrap();
    assert!(out[0].description.contains(r#""status":"unenforced""#));
}

#[test]
fn outgoing_rule_is_marked_enforced() {
    let out = ls_to_os(&process_only_allow()).unwrap();
    assert!(out[0].description.contains(r#""status":"enforced""#));
}

#[test]
fn session_rule_uses_until_restart_duration() {
    let mut ls = process_only_allow();
    ls.permanent = false;
    let out = ls_to_os(&ls).unwrap();
    assert_eq!(out[0].duration, OsDuration::UntilRestart);
}

#[test]
fn blocklist_verdict_becomes_deny_action() {
    let mut ls = process_only_allow();
    ls.verdict = LsVerdict::Blocklist;
    let out = ls_to_os(&ls).unwrap();
    assert_eq!(out[0].action, OsAction::Deny);
}

#[test]
fn protocol_any_is_omitted() {
    let mut ls = process_only_allow();
    ls.scope.protocol = Some(Protocol::Any);
    let out = ls_to_os(&ls).unwrap();
    if let OsOperator::List { operands } = &out[0].operator {
        assert!(!operands.iter().any(|op| matches!(
            op,
            OsOperator::Simple { operand: OsOperand::Protocol, .. }
        )));
    }
}
```

- [ ] **Step 10: Run all the tests**

```bash
cargo test -p snitchwatch-bridge translator::rule_semantics 2>&1 | tail -40
```

Expected: all pass.

- [ ] **Step 11: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/rule_semantics.rs
git commit -m "feat(bridge): LS → OpenSnitch rule translation"
```

> **Note on the reverse direction (`os_to_ls`):** the bridge needs to enumerate opensnitchd rules and present them as LS rules. We defer this to a follow-up task in Part F when we wire up the rules tab. The forward direction is what the M0 spike + the verdict round-trip in M1 needs.

## Task 11: Traffic chart binner

**Files:**
- Modify: `crates/snitchwatch-bridge/src/cache/traffic_bins.rs`

uPlot wants `[timestamps[], values_in[], values_out[]]`. The bridge accumulates per-connection byte counters and bins them into 1-second buckets.

- [ ] **Step 1: Write the test**

```rust
//! Bin per-connection byte counters into uPlot-compatible time buckets.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub struct TrafficSample {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

/// Fixed-size ring buffer of 1-second buckets.
#[derive(Debug)]
pub struct TrafficBinner {
    buckets: VecDeque<TrafficSample>,
    bucket_ms: i64,
    capacity: usize,
}

impl TrafficBinner {
    pub fn new(window_seconds: usize) -> Self {
        Self {
            buckets: VecDeque::with_capacity(window_seconds),
            bucket_ms: 1000,
            capacity: window_seconds,
        }
    }

    pub fn record(&mut self, timestamp_ms: i64, bytes_in: u64, bytes_out: u64) {
        // Round timestamp down to bucket boundary
        let bucket_ts = (timestamp_ms / self.bucket_ms) * self.bucket_ms;

        // Fast path: same bucket as latest
        if let Some(latest) = self.buckets.back_mut() {
            if latest.timestamp_ms == bucket_ts {
                latest.bytes_in = latest.bytes_in.saturating_add(bytes_in);
                latest.bytes_out = latest.bytes_out.saturating_add(bytes_out);
                return;
            }
            if bucket_ts < latest.timestamp_ms {
                // Out-of-order sample — find or create the right bucket.
                // For simplicity in v1, we drop out-of-order samples and log.
                tracing::debug!(bucket_ts, latest_ts = latest.timestamp_ms, "dropped out-of-order traffic sample");
                return;
            }
        }

        // Fill any gap buckets so the chart shows zeros instead of holes
        if let Some(latest) = self.buckets.back() {
            let mut next_ts = latest.timestamp_ms + self.bucket_ms;
            while next_ts < bucket_ts {
                self.push(TrafficSample { timestamp_ms: next_ts, bytes_in: 0, bytes_out: 0 });
                next_ts += self.bucket_ms;
            }
        }

        self.push(TrafficSample { timestamp_ms: bucket_ts, bytes_in, bytes_out });
    }

    fn push(&mut self, sample: TrafficSample) {
        if self.buckets.len() == self.capacity {
            self.buckets.pop_front();
        }
        self.buckets.push_back(sample);
    }

    /// Return the buckets in uPlot format: (timestamps, bytes_in_series, bytes_out_series).
    pub fn series(&self) -> (Vec<i64>, Vec<u64>, Vec<u64>) {
        let mut ts = Vec::with_capacity(self.buckets.len());
        let mut bin = Vec::with_capacity(self.buckets.len());
        let mut bout = Vec::with_capacity(self.buckets.len());
        for s in &self.buckets {
            ts.push(s.timestamp_ms);
            bin.push(s.bytes_in);
            bout.push(s.bytes_out);
        }
        (ts, bin, bout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_in_same_second_aggregate() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 50);
        b.record(1_000_000_000_500, 200, 25);
        let (ts, bi, bo) = b.series();
        assert_eq!(ts, vec![1_000_000_000_000]);
        assert_eq!(bi, vec![300]);
        assert_eq!(bo, vec![75]);
    }

    #[test]
    fn samples_in_different_seconds_make_new_buckets() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 50);
        b.record(1_000_000_001_000, 200, 25);
        let (ts, bi, _) = b.series();
        assert_eq!(ts.len(), 2);
        assert_eq!(bi, vec![100, 200]);
    }

    #[test]
    fn gaps_are_filled_with_zero_buckets() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_000_000, 100, 0);
        b.record(1_000_000_003_000, 50, 0);
        let (ts, bi, _) = b.series();
        assert_eq!(ts.len(), 4, "must fill the 2-second gap: {:?}", ts);
        assert_eq!(bi, vec![100, 0, 0, 50]);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut b = TrafficBinner::new(3);
        for i in 0..5 {
            b.record(1_000_000_000_000 + i * 1000, i as u64, 0);
        }
        let (ts, _, _) = b.series();
        assert_eq!(ts.len(), 3);
        assert_eq!(ts, vec![1_000_000_002_000, 1_000_000_003_000, 1_000_000_004_000]);
    }

    #[test]
    fn out_of_order_samples_are_dropped() {
        let mut b = TrafficBinner::new(60);
        b.record(1_000_000_005_000, 100, 0);
        b.record(1_000_000_002_000, 999, 0); // older — dropped
        let (_, bi, _) = b.series();
        assert!(!bi.contains(&999));
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p snitchwatch-bridge cache::traffic_bins 2>&1 | tail -30
```

Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/cache/traffic_bins.rs
git commit -m "feat(bridge): traffic chart binner with gap filling and ring buffer"
```

---

# Part D — Stateful components

## Task 12: Connection cache + pending-prompt machinery

**Files:**
- Modify: `crates/snitchwatch-bridge/src/cache/connections.rs`

The connection cache holds the rolling history of rows shown in the Connections tab. Pending rows hold the gRPC `AskRule` future open via a `tokio::sync::oneshot` sender. The cache invariant: **pending rows are never evicted by retention policy**, only by user verdict or timeout.

- [ ] **Step 1: Write the cache + tests**

```rust
//! Rolling connection-row buffer with pending-prompt machinery.
//!
//! Invariants:
//!   - Pending rows are never evicted by retention policy
//!   - Eviction is by insertion order, oldest non-pending first
//!   - Each pending row owns a oneshot::Sender that resolves to the verdict;
//!     this is what the gRPC client task awaits before responding to AskRule

use crate::ws_messages::ConnectionRow;
use std::collections::HashMap;
use tokio::sync::oneshot;

#[derive(Debug)]
pub struct PendingHandle {
    pub row_id: String,
    pub sender: oneshot::Sender<Verdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

pub struct ConnectionCache {
    rows: Vec<ConnectionRow>,
    pending: HashMap<String, oneshot::Sender<Verdict>>,
    capacity: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("row not found: {0}")]
    NotFound(String),
    #[error("row {0} is not pending")]
    NotPending(String),
}

impl ConnectionCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            rows: Vec::with_capacity(capacity),
            pending: HashMap::new(),
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Insert a fully-decided row (e.g. from a Ping stats delta).
    pub fn insert_decided(&mut self, row: ConnectionRow) {
        debug_assert!(row.action.is_some(), "use insert_pending for action=None");
        self.rows.push(row);
        self.evict_if_needed();
    }

    /// Insert a pending row and return the receiver side of its verdict
    /// channel. The gRPC client task awaits this receiver before responding
    /// to the AskRule call.
    pub fn insert_pending(&mut self, row: ConnectionRow) -> oneshot::Receiver<Verdict> {
        debug_assert!(row.action.is_none(), "pending rows must have action=None");
        let id = row.id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        self.rows.push(row);
        self.evict_if_needed();
        rx
    }

    /// Resolve a pending row with a verdict. Returns Err if the row isn't
    /// pending.
    pub fn resolve(&mut self, row_id: &str, verdict: Verdict) -> Result<(), CacheError> {
        let sender = self
            .pending
            .remove(row_id)
            .ok_or_else(|| CacheError::NotPending(row_id.to_string()))?;
        // It's fine if the receiver was dropped (e.g. gRPC stream broke).
        let _ = sender.send(verdict);

        // Update the row's action so future re-renders show it as decided.
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == row_id) {
            row.action = Some(match verdict {
                Verdict::Allow => "allow".to_string(),
                Verdict::Deny => "deny".to_string(),
            });
            Ok(())
        } else {
            Err(CacheError::NotFound(row_id.to_string()))
        }
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.pending.keys().cloned().collect()
    }

    pub fn rows(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// Evict oldest non-pending rows until we're at or under capacity.
    fn evict_if_needed(&mut self) {
        while self.rows.len() > self.capacity {
            // Find the first non-pending row (FIFO eviction).
            let idx = self
                .rows
                .iter()
                .position(|r| !self.pending.contains_key(&r.id));
            match idx {
                Some(i) => {
                    self.rows.remove(i);
                }
                None => {
                    // All rows are pending — we cannot evict, capacity is
                    // effectively the pending count. Log a warning so the
                    // operator notices.
                    tracing::warn!(
                        capacity = self.capacity,
                        len = self.rows.len(),
                        "all rows are pending; cache exceeds capacity"
                    );
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided_row(id: &str, action: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            process: "p".to_string(),
            process_path: None,
            dst_host: "h".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: Some(action.to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        }
    }

    fn pending_row(id: &str) -> ConnectionRow {
        let mut r = decided_row(id, "allow");
        r.action = None;
        r
    }

    #[test]
    fn insert_decided_grows_the_cache() {
        let mut c = ConnectionCache::new(10);
        c.insert_decided(decided_row("a", "allow"));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn eviction_removes_oldest_non_pending_first() {
        let mut c = ConnectionCache::new(2);
        c.insert_decided(decided_row("a", "allow"));
        c.insert_decided(decided_row("b", "allow"));
        c.insert_decided(decided_row("c", "allow"));
        let ids: Vec<&str> = c.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
    }

    #[test]
    fn pending_rows_are_not_evicted() {
        let mut c = ConnectionCache::new(2);
        c.insert_decided(decided_row("a", "allow"));
        let _rx = c.insert_pending(pending_row("p1"));
        c.insert_decided(decided_row("c", "allow"));
        let ids: Vec<&str> = c.rows().iter().map(|r| r.id.as_str()).collect();
        // "a" was evicted (oldest non-pending); "p1" stayed.
        assert_eq!(ids, vec!["p1", "c"]);
    }

    #[tokio::test]
    async fn resolve_fires_oneshot_and_updates_row() {
        let mut c = ConnectionCache::new(10);
        let rx = c.insert_pending(pending_row("p1"));
        c.resolve("p1", Verdict::Allow).unwrap();
        let received = rx.await.unwrap();
        assert_eq!(received, Verdict::Allow);
        assert_eq!(c.rows()[0].action.as_deref(), Some("allow"));
    }

    #[test]
    fn resolve_unknown_row_errors() {
        let mut c = ConnectionCache::new(10);
        let err = c.resolve("nope", Verdict::Allow).unwrap_err();
        assert!(matches!(err, CacheError::NotPending(_)));
    }

    #[test]
    fn cache_can_exceed_capacity_when_all_rows_pending() {
        let mut c = ConnectionCache::new(2);
        let _r1 = c.insert_pending(pending_row("p1"));
        let _r2 = c.insert_pending(pending_row("p2"));
        let _r3 = c.insert_pending(pending_row("p3"));
        // No eviction possible — pending rows are sacred.
        assert_eq!(c.len(), 3);
    }
}
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p snitchwatch-bridge cache::connections 2>&1 | tail -30
```

Expected: all pass.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/cache/connections.rs
git commit -m "feat(bridge): connection cache with pending-prompt oneshots"
```

---

# Part E — I/O layers

## Task 13: gRPC client with reconnect loop

**Files:**
- Modify: `crates/snitchwatch-bridge/src/grpc_client.rs`

This task wires the bridge to opensnitchd. The exact RPC method names depend on what your `snitchwatch-proto` build generated; cross-reference Task 4's findings in `docs/m0-spike-findings.md`.

- [ ] **Step 1: Implement the client**

```rust
//! gRPC client to opensnitchd, with exponential-backoff reconnect.

use crate::error::BridgeError;
use snitchwatch_proto::ui::ui_client::UiClient;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::transport::{Channel, Endpoint};
use tracing::{info, warn};

pub struct GrpcClient {
    endpoint: String,
}

impl GrpcClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self { endpoint: endpoint.into() }
    }

    /// Connect with exponential backoff. Returns the connected channel.
    pub async fn connect_with_backoff(&self) -> Result<Channel, BridgeError> {
        let mut delay = Duration::from_millis(500);
        let max_delay = Duration::from_secs(60);

        loop {
            match Endpoint::from_shared(self.endpoint.clone())
                .map_err(|e| BridgeError::Transport(tonic::transport::Error::from(e)))?
                .connect_timeout(Duration::from_secs(3))
                .connect()
                .await
            {
                Ok(channel) => {
                    info!(endpoint = %self.endpoint, "connected to opensnitchd");
                    return Ok(channel);
                }
                Err(e) => {
                    warn!(error = %e, retry_in_ms = delay.as_millis(), "gRPC connect failed, retrying");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(max_delay);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_endpoint_eventually_errors() {
        // We can't easily test the infinite loop, but we can test that
        // a malformed endpoint surfaces an error before retry.
        let client = GrpcClient::new("not-a-url");
        let result = Endpoint::from_shared(client.endpoint.clone());
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Build & run the test**

```bash
cargo test -p snitchwatch-bridge grpc_client 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/grpc_client.rs
git commit -m "feat(bridge): gRPC client connect-with-backoff helper"
```

> **Followup:** the actual notification stream subscription (translating opensnitchd `Notification` events into `ServerMessage` and feeding them to the WS server) is wired up in Task 16 (downstream translator) once we have a clearer picture from the M0 spike about the exact stream RPC.

## Task 14: WebSocket server skeleton

**Files:**
- Modify: `crates/snitchwatch-bridge/src/ws_server.rs`

- [ ] **Step 1: Implement the server**

```rust
//! WebSocket server for the embedded webview.
//!
//! Binds to a configurable local address (default: a random ephemeral port
//! on 127.0.0.1) and serves the `/stream` endpoint. The Tauri shell reads
//! the actual bound port after startup and points the webview at it.

use crate::ws_messages::{ClientMessage, ServerMessage};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info};

/// Channels the bridge core uses to talk to the WS server.
#[derive(Clone)]
pub struct WsHandles {
    /// Server pushes broadcast to all connected clients.
    pub broadcast: broadcast::Sender<ServerMessage>,
    /// Inbound client messages get forwarded here for the bridge to act on.
    pub inbound: mpsc::Sender<ClientMessage>,
}

pub struct WsServer {
    bind: SocketAddr,
    handles: WsHandles,
}

impl WsServer {
    pub fn new(bind: SocketAddr, handles: WsHandles) -> Self {
        Self { bind, handles }
    }

    /// Bind the listener and return the actual bound address (so callers can
    /// pass `:0` for an ephemeral port and discover what they got).
    pub async fn bind(&self) -> std::io::Result<(TcpListener, SocketAddr)> {
        let listener = TcpListener::bind(self.bind).await?;
        let local = listener.local_addr()?;
        Ok((listener, local))
    }

    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        let app = Router::new()
            .route("/stream", get(ws_handler))
            .with_state(self.handles);

        info!(addr = ?listener.local_addr()?, "WS server starting");
        axum::serve(listener, app).await
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(handles): axum::extract::State<WsHandles>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, handles))
}

async fn handle_socket(socket: WebSocket, handles: WsHandles) {
    use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();
    let mut broadcast_rx = handles.broadcast.subscribe();

    // Outbound task: forward broadcast messages to this client.
    let outbound = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    error!(error = %e, "failed to serialize ServerMessage");
                    continue;
                }
            };
            if sender.send(Message::Text(json.into())).await.is_err() {
                debug!("WS client disconnected (outbound)");
                break;
            }
        }
    });

    // Inbound loop: parse client messages and forward to the bridge.
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(parsed) => {
                        if handles.inbound.send(parsed).await.is_err() {
                            debug!("inbound channel closed; dropping client");
                            break;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, raw = %text, "failed to parse ClientMessage");
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    outbound.abort();
    debug!("WS client connection ended");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_binds_to_ephemeral_port() {
        let (broadcast_tx, _) = broadcast::channel(16);
        let (inbound_tx, _) = mpsc::channel(16);
        let handles = WsHandles { broadcast: broadcast_tx, inbound: inbound_tx };
        let server = WsServer::new("127.0.0.1:0".parse().unwrap(), handles);
        let (_listener, addr) = server.bind().await.unwrap();
        assert_ne!(addr.port(), 0, "ephemeral port should resolve to a real port");
    }
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p snitchwatch-bridge ws_server 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/ws_server.rs
git commit -m "feat(bridge): WebSocket server with broadcast/inbound channels"
```

---

# Part F — Mock daemon and integration tests

## Task 15: Build mock_opensnitchd

**Files:**
- Create: `tests/mock_opensnitchd/Cargo.toml`
- Create: `tests/mock_opensnitchd/src/lib.rs`

The mock implements opensnitchd's gRPC server interface and lets tests script event sequences. It runs in-process so no container, no root, no kernel.

- [ ] **Step 1: Create the directory**

```bash
mkdir -p tests/mock_opensnitchd/src
```

- [ ] **Step 2: Write `tests/mock_opensnitchd/Cargo.toml`**

```toml
[package]
name = "mock-opensnitchd"
version.workspace = true
edition.workspace = true
license.workspace = true

[lib]
path = "src/lib.rs"

[dependencies]
snitchwatch-proto = { path = "../../crates/snitchwatch-proto" }
tokio = { workspace = true }
tonic = { workspace = true }
prost = { workspace = true }
tracing = { workspace = true }
tokio-stream = "0.1"
async-stream = "0.3"
```

- [ ] **Step 3: Write `tests/mock_opensnitchd/src/lib.rs`**

The exact trait name and methods depend on the proto. After Task 3 generated the stubs, the server-side trait is in `snitchwatch_proto::ui::ui_server::Ui`. The methods on it match the RPCs in `ui.proto`. **Cross-reference your m0-spike-findings.md notes for the actual method names.**

```rust
//! In-process mock of opensnitchd's gRPC server.
//!
//! The mock implements the `Ui` trait from the generated proto stubs and
//! takes scripted event sequences via a public API. Tests construct a mock,
//! script some events, hand the gRPC server's address to the bridge under
//! test, and assert on the resulting WebSocket message stream.
//!
//! # Adapting to your proto version
//!
//! The exact methods on the `Ui` trait depend on the version of opensnitch
//! you vendored in `vendor/opensnitch`. Run:
//!
//!     cargo doc -p snitchwatch-proto --open
//!
//! and look at `ui::ui_server::Ui` to see the trait you need to implement.
//! Most opensnitch versions expose at least:
//!   - `notifications` (bidi stream of NotificationReply ↔ Notification)
//!   - `ping` (unary, returns PingReply with stats)
//!   - `ask_rule` (sometimes a separate unary; sometimes folded into notifications)

use snitchwatch_proto::ui::{
    ui_server::{Ui, UiServer},
    Notification, NotificationReply, PingRequest, PingReply,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::{transport::Server, Request, Response, Status, Streaming};

/// Scripted event the mock can deliver to the bridge.
#[derive(Debug, Clone)]
pub enum ScriptedEvent {
    /// Send a Notification with the given payload to the connected client.
    Notification(Notification),
    /// Wait this many milliseconds before delivering the next event.
    Delay(u64),
    /// Forcibly close the stream (simulates daemon crash / network drop).
    Disconnect,
}

#[derive(Default)]
pub struct MockState {
    pub scripted: Vec<ScriptedEvent>,
    pub received_replies: Vec<NotificationReply>,
}

#[derive(Clone)]
pub struct MockOpensnitchd {
    state: Arc<Mutex<MockState>>,
}

impl MockOpensnitchd {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(MockState::default())) }
    }

    pub async fn script(&self, events: Vec<ScriptedEvent>) {
        self.state.lock().await.scripted = events;
    }

    pub async fn received_replies(&self) -> Vec<NotificationReply> {
        self.state.lock().await.received_replies.clone()
    }

    pub fn into_server(self) -> UiServer<Self> {
        UiServer::new(self)
    }

    /// Spawn the mock on a random local port and return its address.
    pub async fn spawn(self) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = self.into_server();

        tokio::spawn(async move {
            Server::builder()
                .add_service(server)
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
                .await
                .unwrap();
        });

        // Brief pause to let the server actually start accepting connections.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        addr
    }
}

#[tonic::async_trait]
impl Ui for MockOpensnitchd {
    type NotificationsStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<Notification, Status>> + Send + 'static>,
    >;

    async fn notifications(
        &self,
        request: Request<Streaming<NotificationReply>>,
    ) -> Result<Response<Self::NotificationsStream>, Status> {
        let state = self.state.clone();
        let mut inbound = request.into_inner();

        // Spawn an inbound collector that records every reply the bridge sends.
        let collector_state = state.clone();
        tokio::spawn(async move {
            while let Ok(Some(reply)) = inbound.message().await {
                collector_state.lock().await.received_replies.push(reply);
            }
        });

        // Outbound: deliver scripted events.
        let outbound_state = state.clone();
        let outbound = async_stream::try_stream! {
            let scripted: Vec<ScriptedEvent> = {
                outbound_state.lock().await.scripted.clone()
            };
            for event in scripted {
                match event {
                    ScriptedEvent::Notification(n) => yield n,
                    ScriptedEvent::Delay(ms) => {
                        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    }
                    ScriptedEvent::Disconnect => {
                        // Closing the stream simulates a disconnect.
                        return;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(outbound)))
    }

    async fn ping(
        &self,
        _request: Request<PingRequest>,
    ) -> Result<Response<PingReply>, Status> {
        // Default empty ping reply. Tests that need stats override the
        // `ping` method by wrapping the mock or extending state.
        Ok(Response::new(PingReply::default()))
    }
}
```

> **Adaptation note:** if your proto's RPCs are named differently (e.g. `Subscribe` instead of `Notifications`, or `AskRule` is its own RPC), edit the trait method signatures to match. The pattern stays the same: implement each method, push events from `state.scripted`, record incoming requests into `state.received_*`. The compile errors from `cargo build -p mock-opensnitchd` will tell you exactly what's missing.

- [ ] **Step 4: Build the mock**

```bash
cargo build -p mock-opensnitchd 2>&1 | tail -30
```

Expected: probably some compile errors because `Notification`, `PingRequest`, `PingReply` field names don't exist in your proto version. Fix them to match. Iterate until clean.

- [ ] **Step 5: Commit**

```bash
git add tests/mock_opensnitchd
git commit -m "feat(test): in-process mock_opensnitchd gRPC server"
```

## Task 16: Wire downstream translator (gRPC events → WS messages)

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/downstream.rs`

This task takes opensnitchd `Notification` events and produces `ServerMessage` outputs for the WS server to broadcast.

- [ ] **Step 1: Implement and test together (small surface area)**

```rust
//! Translate opensnitchd Notification events into LS WebSocket ServerMessages.

use crate::cache::connections::ConnectionCache;
use crate::ws_messages::{ConnectionRow, ServerMessage};
use snitchwatch_proto::ui::Notification;

/// Outcome of translating one Notification.
pub enum Translated {
    /// Push these messages to the WS broadcast.
    Messages(Vec<ServerMessage>),
    /// This notification was an AskRule and the bridge should call
    /// `cache.insert_pending` with the produced row, then await the
    /// resulting oneshot before replying.
    AskRule(ConnectionRow),
    /// Notification not relevant to the UI; ignore.
    Ignored,
}

/// Translate one Notification. The exact discriminator field depends on the
/// proto — adapt to your version.
pub fn translate_notification(n: &Notification) -> Translated {
    // Pseudocode skeleton: match on n.r#type / n.kind / whatever the proto
    // calls it. Real implementation depends on what the M0 spike found.
    //
    // For the bridge's first cut we handle three cases:
    //   - AskRule notifications → produce a pending ConnectionRow
    //   - Connection event notifications (allowed/denied flow logged) →
    //     produce a decided ConnectionRow + InsertConnectionRows message
    //   - Everything else (ping stats, rule changes) → Ignored for now
    //
    // Once we get more comfortable with the proto, expand to handle stats
    // updates (TrafficEvents) and rule list changes (SetRules).

    let _ = n; // silence until we wire this up
    Translated::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_notification_returns_ignored() {
        // Construct a default Notification — fields depend on proto.
        let n = Notification::default();
        match translate_notification(&n) {
            Translated::Ignored => {}
            _ => panic!("expected Ignored"),
        }
    }
}
```

- [ ] **Step 2: Build and run**

```bash
cargo test -p snitchwatch-bridge translator::downstream 2>&1 | tail -20
```

Expected: passes (the trivial case).

- [ ] **Step 3: Commit the skeleton**

```bash
git add crates/snitchwatch-bridge/src/translator/downstream.rs
git commit -m "feat(bridge): downstream translator skeleton"
```

> **NOTE:** the full downstream translation logic is intentionally minimal in this plan. The reason is that the *exact* shape of opensnitchd notifications is one of the things the M0 spike is supposed to teach us. After the spike, expand this module by adding one match arm per notification type, with a unit test per arm. Use the `ScriptedEvent::Notification(...)` mechanism in `mock_opensnitchd` to drive integration tests for each shape. **Do not block on getting this complete in Plan 1** — the goal here is the round-trip plumbing, not exhaustive event coverage. Plan 1 succeeds when AskRule round-trips end-to-end and at least one decided-row notification produces an InsertConnectionRows message.

## Task 17: Wire upstream router (WS sendAction → gRPC calls)

**Files:**
- Modify: `crates/snitchwatch-bridge/src/translator/upstream.rs`

- [ ] **Step 1: Implement**

```rust
//! Route WS ClientMessages from the UI into gRPC actions on the daemon.

use crate::cache::connections::{ConnectionCache, Verdict};
use crate::ws_messages::{ClientMessage, VerdictAction};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("cache error: {0}")]
    Cache(#[from] crate::cache::connections::CacheError),
}

/// Apply a ClientMessage to the bridge's state.
///
/// The cache is mutated synchronously here. Side effects that need to talk
/// to gRPC (e.g. AddRule → ChangeRule call) are handled by the caller after
/// this function returns.
pub fn apply(
    cache: &mut ConnectionCache,
    msg: ClientMessage,
) -> Result<UpstreamEffect, RouterError> {
    match msg {
        ClientMessage::SetVerdict { row_id, verdict, scope: _, remember } => {
            let v = match verdict {
                VerdictAction::Allow => Verdict::Allow,
                VerdictAction::Deny => Verdict::Deny,
            };
            cache.resolve(&row_id, v)?;
            Ok(UpstreamEffect::VerdictApplied { row_id, verdict: v, remember })
        }
        ClientMessage::AddRule { rule } => Ok(UpstreamEffect::AddRule { rule }),
        ClientMessage::DeleteRule { rule_id } => Ok(UpstreamEffect::DeleteRule { rule_id }),
        ClientMessage::UpdateRule { rule_id, rule } => Ok(UpstreamEffect::UpdateRule { rule_id, rule }),
        _ => Ok(UpstreamEffect::None),
    }
}

/// Side effect the caller (the bridge orchestrator) should perform.
#[derive(Debug, Clone, PartialEq)]
pub enum UpstreamEffect {
    None,
    VerdictApplied { row_id: String, verdict: Verdict, remember: bool },
    AddRule { rule: serde_json::Value },
    DeleteRule { rule_id: String },
    UpdateRule { rule_id: String, rule: serde_json::Value },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws_messages::{ConnectionRow, VerdictScope};

    fn make_pending(cache: &mut ConnectionCache, id: &str) {
        let _ = cache.insert_pending(ConnectionRow {
            id: id.to_string(),
            process: "p".to_string(),
            process_path: None,
            dst_host: "h".to_string(),
            dst_ip: "1.1.1.1".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: None,
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 0,
        });
    }

    #[test]
    fn set_verdict_resolves_pending_row() {
        let mut cache = ConnectionCache::new(10);
        make_pending(&mut cache, "p1");
        let effect = apply(
            &mut cache,
            ClientMessage::SetVerdict {
                row_id: "p1".to_string(),
                verdict: VerdictAction::Allow,
                scope: VerdictScope::ThisHost,
                remember: false,
            },
        )
        .unwrap();
        match effect {
            UpstreamEffect::VerdictApplied { row_id, verdict, remember } => {
                assert_eq!(row_id, "p1");
                assert_eq!(verdict, Verdict::Allow);
                assert!(!remember);
            }
            other => panic!("unexpected effect: {:?}", other),
        }
        assert!(cache.pending_ids().is_empty());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p snitchwatch-bridge translator::upstream 2>&1 | tail -20
```

Expected: passes.

- [ ] **Step 3: Commit**

```bash
git add crates/snitchwatch-bridge/src/translator/upstream.rs
git commit -m "feat(bridge): upstream router for WS sendAction → gRPC effects"
```

## Task 18: End-to-end integration test against mock daemon

**Files:**
- Create: `tests/bridge_protocol_test.rs`

This is the test that proves the whole bridge works. It spins up a mock daemon, starts the bridge pointing at it, opens a real WebSocket client to the bridge, scripts an AskRule notification, asserts the WS client sees a pending row, sends a verdict, asserts the mock receives the verdict reply.

- [ ] **Step 1: Add the test file as a workspace integration test**

We make `tests/bridge_protocol_test.rs` a binary in its own little crate. Add a wrapper crate:

```bash
mkdir -p tests/integration/src
```

Write `tests/integration/Cargo.toml`:

```toml
[package]
name = "snitchwatch-integration-tests"
version.workspace = true
edition.workspace = true
license.workspace = true

[[test]]
name = "bridge_protocol"
path = "../bridge_protocol_test.rs"

[dev-dependencies]
snitchwatch-bridge = { path = "../../crates/snitchwatch-bridge" }
snitchwatch-proto = { path = "../../crates/snitchwatch-proto" }
mock-opensnitchd = { path = "../mock_opensnitchd" }
tokio = { workspace = true }
tokio-tungstenite = { workspace = true }
futures-util = { workspace = true }
serde_json = { workspace = true }
tracing-subscriber = { workspace = true }
```

Add to root `Cargo.toml` workspace members:

```toml
members = [
    "crates/snitchwatch-proto",
    "crates/snitchwatch-spike",
    "crates/snitchwatch-bridge",
    "crates/snitchwatch-bridge-cli",
    "tests/mock_opensnitchd",
    "tests/integration",   # add this
]
```

- [ ] **Step 2: Write the test**

`tests/bridge_protocol_test.rs`:

```rust
//! End-to-end test: mock_opensnitchd ↔ bridge ↔ WebSocket client.
//!
//! This is the M1 acceptance test. It proves that:
//!   1. The bridge can connect to a gRPC server
//!   2. AskRule notifications produce pending WS rows
//!   3. WS verdicts close the loop and the mock observes the reply
//!
//! Adapt the AskRule construction to match your proto's actual fields.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::{MockOpensnitchd, ScriptedEvent};
use snitchwatch_proto::ui::Notification;

#[tokio::test]
async fn ask_rule_round_trip() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Spawn mock daemon with one AskRule scripted.
    let mock = MockOpensnitchd::new();
    let ask = Notification::default(); // populate fields per your proto
    mock.script(vec![ScriptedEvent::Notification(ask)]).await;
    let mock_addr = mock.clone().spawn().await;

    // 2. Start the bridge pointing at the mock.
    //    (Plan 1's bridge orchestrator binary doesn't exist yet — we wire it
    //    in Task 19. For this test we manually drive the pieces.)

    // 3. Open a WebSocket client and observe.
    //    Once Task 19 lands, this test connects via tokio_tungstenite to the
    //    bridge's WS endpoint, awaits an `insertConnectionRows` message, and
    //    sends back a `setVerdict`.

    // For Plan 1's first cut, this test verifies only that the mock spawns
    // and accepts a gRPC connection. The full round-trip becomes the
    // success criterion at the end of Task 19.

    let endpoint = format!("http://{}", mock_addr);
    let result = tonic::transport::Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await;
    assert!(result.is_ok(), "mock should accept a tonic client connection");
}
```

- [ ] **Step 3: Run the test**

```bash
cargo test -p snitchwatch-integration-tests 2>&1 | tail -30
```

Expected: passes. (This first cut is intentionally minimal — Task 19 wires up the full round-trip.)

- [ ] **Step 4: Commit**

```bash
git add tests/integration tests/bridge_protocol_test.rs Cargo.toml
git commit -m "feat(test): integration test scaffold against mock_opensnitchd"
```

## Task 19: Bridge CLI orchestrator

**Files:**
- Create: `crates/snitchwatch-bridge-cli/Cargo.toml`
- Create: `crates/snitchwatch-bridge-cli/src/main.rs`

The CLI ties everything together: connect gRPC client to daemon, start WS server, route messages between them via the cache and the translator. This is what subsequent integration tests will spawn (and what Plan 2 will eventually wrap in Tauri).

- [ ] **Step 1: Create the crate**

```bash
mkdir -p crates/snitchwatch-bridge-cli/src
```

`crates/snitchwatch-bridge-cli/Cargo.toml`:

```toml
[package]
name = "snitchwatch-bridge-cli"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
snitchwatch-bridge = { path = "../snitchwatch-bridge" }
snitchwatch-proto = { path = "../snitchwatch-proto" }
tokio = { workspace = true }
tonic = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
anyhow = { workspace = true }
```

- [ ] **Step 2: Write `crates/snitchwatch-bridge-cli/src/main.rs`**

```rust
//! Bridge CLI — runs the bridge against either real opensnitchd or the mock.
//!
//! Usage:
//!   snitchwatch-bridge-cli [--grpc URL] [--ws-bind ADDR]
//!
//! Defaults:
//!   --grpc http://127.0.0.1:50051
//!   --ws-bind 127.0.0.1:0     (random ephemeral port — printed to stdout)

use anyhow::Result;
use snitchwatch_bridge::cache::connections::ConnectionCache;
use snitchwatch_bridge::grpc_client::GrpcClient;
use snitchwatch_bridge::translator::{downstream, upstream};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge::ws_server::{WsHandles, WsServer};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let grpc_url = std::env::var("SNITCHWATCH_GRPC")
        .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string());
    let ws_bind = std::env::var("SNITCHWATCH_WS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_string());

    info!(%grpc_url, %ws_bind, "starting snitchwatch-bridge-cli");

    // Channels
    let (broadcast_tx, _) = broadcast::channel::<ServerMessage>(256);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<ClientMessage>(256);

    // Shared cache
    let cache = Arc::new(Mutex::new(ConnectionCache::new(10_000)));

    // WS server
    let ws_handles = WsHandles {
        broadcast: broadcast_tx.clone(),
        inbound: inbound_tx,
    };
    let ws_server = WsServer::new(ws_bind.parse()?, ws_handles);
    let (listener, ws_addr) = ws_server.bind().await?;
    println!("WS_LISTEN_ADDR={}", ws_addr); // machine-parseable for tests
    tokio::spawn(async move {
        if let Err(e) = ws_server.serve(listener).await {
            error!(error = %e, "ws_server::serve exited");
        }
    });

    // gRPC client
    let client = GrpcClient::new(grpc_url);
    let _channel = client.connect_with_backoff().await?;

    // TODO (post-spike): subscribe to the daemon's notification stream and
    // pump events through `downstream::translate_notification` into
    // `broadcast_tx`. The exact subscription RPC depends on the proto.

    // Inbound loop: drain WS client messages and apply them.
    let cache_for_inbound = cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let mut cache = cache_for_inbound.lock().await;
            match upstream::apply(&mut cache, msg) {
                Ok(effect) => {
                    info!(?effect, "applied upstream effect");
                    // TODO: side effects that talk to gRPC (AddRule etc.)
                }
                Err(e) => error!(error = %e, "upstream apply failed"),
            }
        }
    });

    // Park forever (or until Ctrl-C)
    tokio::signal::ctrl_c().await?;
    info!("shutdown signal received");
    Ok(())
}
```

- [ ] **Step 3: Build it**

```bash
cargo build -p snitchwatch-bridge-cli 2>&1 | tail -20
```

Expected: builds clean.

- [ ] **Step 4: Smoke test by running it**

In one terminal, start the mock_opensnitchd via a tiny helper. We don't have a binary for the mock yet, so the easiest smoke test is the integration test from Task 18. Run that and watch the logs:

```bash
RUST_LOG=snitchwatch_bridge=debug cargo test -p snitchwatch-integration-tests -- --nocapture 2>&1 | tail -50
```

Expected: integration test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/snitchwatch-bridge-cli
git commit -m "feat(bridge): orchestrator CLI binary"
```

## Task 20: Wire the full AskRule round trip

**Files:**
- Modify: `tests/bridge_protocol_test.rs`
- Modify: `crates/snitchwatch-bridge-cli/src/main.rs`
- Modify: `crates/snitchwatch-bridge/src/translator/downstream.rs`

This task closes the loop. After this lands, you can script an AskRule on the mock, watch the WS client receive a pending row, send a verdict, and watch the mock receive the reply. **This is M1's acceptance criterion.**

Because this task depends heavily on the actual proto field names (which the M0 spike taught you), the steps are deliberately less prescriptive. Use the M0 spike findings document as your guide.

- [ ] **Step 1: In `downstream.rs`, expand `translate_notification` to handle AskRule**

Pseudocode (adapt to your proto):

```rust
pub fn translate_notification(n: &Notification) -> Translated {
    use snitchwatch_proto::ui::NotificationType; // or whatever enum your proto uses
    match n.r#type() {
        NotificationType::AskRule => {
            // Parse the connection payload (JSON or protobuf-embedded)
            let row = ConnectionRow {
                id: format!("ask-{}", n.id),
                // ... fill from n.data ...
                action: None,
                started_at_ms: now_ms(),
                ..Default::default()
            };
            Translated::AskRule(row)
        }
        NotificationType::Connection => {
            // Decided-row insert
            let row = ConnectionRow { /* ... */ };
            Translated::Messages(vec![ServerMessage::InsertConnectionRows { rows: vec![row] }])
        }
        _ => Translated::Ignored,
    }
}
```

Add a unit test that constructs a synthetic `Notification` with the AskRule discriminator and asserts the translator returns `Translated::AskRule(...)`.

- [ ] **Step 2: In `bridge-cli/main.rs`, wire the notification stream pump**

After `connect_with_backoff` returns the channel, build a `UiClient::new(channel)`, open the bidirectional notifications stream, and run a loop that:

1. Receives a `Notification` from the inbound stream
2. Calls `downstream::translate_notification`
3. Acts on the result:
   - `Translated::Messages(msgs)` → `broadcast_tx.send(msg)` for each
   - `Translated::AskRule(row)` → `cache.insert_pending(row)`, then await the resulting oneshot, then send a `NotificationReply` back through the outbound side of the stream with the verdict
   - `Translated::Ignored` → log and continue

The verdict reply needs to correlate to the AskRule's `id` field. Store `(notification_id → row_id)` in a small `HashMap` so you can build the right reply when the oneshot fires.

This is genuinely intricate code — write it carefully and lean on the type system.

- [ ] **Step 3: In `bridge_protocol_test.rs`, expand the round-trip test**

```rust
#[tokio::test]
async fn ask_rule_round_trip_full() {
    let _ = tracing_subscriber::fmt::try_init();

    // 1. Spawn mock daemon with one AskRule scripted
    let mock = MockOpensnitchd::new();
    let ask = build_ask_rule_notification("/usr/bin/curl", "example.com", 443);
    mock.script(vec![ScriptedEvent::Notification(ask)]).await;
    let mock_addr = mock.clone().spawn().await;

    // 2. Start the bridge as a tokio task pointing at the mock
    std::env::set_var("SNITCHWATCH_GRPC", format!("http://{}", mock_addr));
    std::env::set_var("SNITCHWATCH_WS_BIND", "127.0.0.1:0");
    // For now, drive bridge components manually because spawning the CLI
    // binary from a test is awkward. Refactor `bridge-cli` so its `run()`
    // function is callable from tests.

    // ...

    // 3. Open WS client, await `insertConnectionRows`, send `setVerdict`
    // 4. Assert mock.received_replies() contains the matching reply

    // (This is the M1 acceptance test — make it pass and you've built the
    // bridge.)
}

fn build_ask_rule_notification(process: &str, host: &str, port: u16) -> Notification {
    // Adapt to your proto fields.
    let mut n = Notification::default();
    // n.r#type = NotificationType::AskRule as i32;
    // n.id = 1;
    // n.data = serde_json::to_string(&serde_json::json!({
    //     "process": process, "host": host, "port": port
    // })).unwrap();
    n
}
```

- [ ] **Step 4: Run the test until it passes**

```bash
cargo test -p snitchwatch-integration-tests ask_rule_round_trip_full -- --nocapture 2>&1 | tail -50
```

Expected (after iteration): passes.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(bridge): full AskRule round-trip mock ↔ bridge ↔ WS client"
```

> **🎉 M1 acceptance criterion met when this test passes.** The bridge translates a daemon-side AskRule into a pending WebSocket row, accepts a verdict from a WebSocket client, and the daemon observes the reply. From here, every subsequent feature is incremental — adding more notification types to the downstream translator, adding more `ClientMessage` variants to the upstream router, and so on.

---

# Part G — Polish

## Task 21: Refactor `bridge-cli` so its `run` function is testable

**Files:**
- Modify: `crates/snitchwatch-bridge-cli/src/main.rs`
- Create: `crates/snitchwatch-bridge-cli/src/lib.rs`

- [ ] **Step 1: Move the orchestration logic into `lib.rs`**

```rust
// crates/snitchwatch-bridge-cli/src/lib.rs

use anyhow::Result;
use std::net::SocketAddr;

pub struct BridgeConfig {
    pub grpc_url: String,
    pub ws_bind: SocketAddr,
}

pub struct RunningBridge {
    pub ws_addr: SocketAddr,
    /// Drop this handle to shut down the bridge.
    pub shutdown: tokio::sync::oneshot::Sender<()>,
}

pub async fn run(config: BridgeConfig) -> Result<RunningBridge> {
    // Move all the wiring from main.rs into here.
    // Return immediately after the WS server is listening so tests can
    // connect to ws_addr without polling.
    todo!("move main.rs wiring here")
}
```

Then `main.rs` becomes:

```rust
use anyhow::Result;
use snitchwatch_bridge_cli::{run, BridgeConfig};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let config = BridgeConfig {
        grpc_url: std::env::var("SNITCHWATCH_GRPC")
            .unwrap_or_else(|_| "http://127.0.0.1:50051".to_string()),
        ws_bind: std::env::var("SNITCHWATCH_WS_BIND")
            .unwrap_or_else(|_| "127.0.0.1:0".to_string())
            .parse()?,
    };
    let running = run(config).await?;
    println!("WS_LISTEN_ADDR={}", running.ws_addr);
    tokio::signal::ctrl_c().await?;
    let _ = running.shutdown.send(());
    Ok(())
}
```

- [ ] **Step 2: Update integration tests to use `run()` directly**

```rust
let running = snitchwatch_bridge_cli::run(BridgeConfig {
    grpc_url: format!("http://{}", mock_addr),
    ws_bind: "127.0.0.1:0".parse().unwrap(),
}).await.unwrap();

let ws_url = format!("ws://{}/stream", running.ws_addr);
let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
// ... drive the test ...
let _ = running.shutdown.send(());
```

- [ ] **Step 3: Run the full test suite**

```bash
just test 2>&1 | tail -40
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/snitchwatch-bridge-cli
git commit -m "refactor(bridge-cli): expose run() as library function for testing"
```

## Task 22: Final polish — README, justfile recipes, lints

**Files:**
- Modify: `README.md`
- Modify: `justfile`

- [ ] **Step 1: Expand `README.md`**

```markdown
# Snitchwatch

A Little Snitch–style network firewall GUI for Linux, on top of OpenSnitch.

## Status

Pre-alpha. Plan 1 (bridge foundation) is complete: the headless bridge
crate translates between OpenSnitch's gRPC protocol and Little Snitch's
WebSocket protocol, with full test coverage and an AskRule round-trip.

See `docs/superpowers/specs/2026-04-10-snitchwatch-design.md` for the design.

## Building

Requires: Rust 1.75+, protobuf compiler (`protoc`).

```bash
git submodule update --init --recursive
just build
```

## Testing

```bash
just test                    # all tests, including the mock-daemon integration tests
just test-bridge             # just the bridge crate's unit tests
```

## Running the bridge against real opensnitchd

Start opensnitchd in a podman container, then:

```bash
podman run -d --rm \
    --name opensnitchd-dev \
    --privileged --network=host --pid=host \
    --cap-add=NET_ADMIN,SYS_ADMIN,BPF \
    docker.io/evilsocket/opensnitch:latest

cargo run -p snitchwatch-bridge-cli
```

The bridge prints `WS_LISTEN_ADDR=127.0.0.1:NNNNN` to stdout.
You can poke it with `websocat`:

```bash
websocat ws://127.0.0.1:NNNNN/stream
```

## Layout

See `docs/superpowers/plans/2026-04-10-bridge-foundation.md` for the
architecture and the full crate-by-crate explanation.

## License

GPL-2.0
```

- [ ] **Step 2: Expand `justfile`**

```just
default:
    @just --list

build:
    cargo build --workspace

test:
    cargo test --workspace

test-bridge:
    cargo test -p snitchwatch-bridge

check:
    cargo check --workspace
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo fmt --all

regen-proto:
    cargo build -p snitchwatch-proto

run-bridge:
    RUST_LOG=info cargo run -p snitchwatch-bridge-cli

run-spike endpoint="http://127.0.0.1:50051":
    RUST_LOG=info cargo run -p snitchwatch-spike -- {{endpoint}}
```

- [ ] **Step 3: Run the lint pass**

```bash
just check 2>&1 | tail -40
```

Expected: no clippy warnings. Fix anything that pops up.

- [ ] **Step 4: Commit**

```bash
git add README.md justfile
git commit -m "docs: README + justfile recipes for Plan 1 deliverables"
```

---

# Plan 1 acceptance criteria

You're done with Plan 1 when **all** of these are true:

- [ ] `just test` passes from a clean clone (after `git submodule update --init`)
- [ ] `crates/snitchwatch-bridge` has 80%+ line coverage on translator and cache modules (check with `cargo tarpaulin -p snitchwatch-bridge` if you have it installed; otherwise eyeball)
- [ ] `docs/m0-spike-findings.md` exists and recommends PROCEED
- [ ] The integration test `ask_rule_round_trip_full` passes — meaning the bridge can take an AskRule from a mock daemon, surface it as a pending WS row, accept a verdict, and the mock observes the reply
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `crates/snitchwatch-bridge-cli` runs against real opensnitchd without crashing for at least 60s
- [ ] No TODO comments in committed code that block Plan 2 (followups in `ws_messages.rs` about real LS payload field names are acceptable; they get resolved in Plan 2 when we wire the real UI)

When all of these check, **stop and come back to write Plan 2 (vendored UI)**.
