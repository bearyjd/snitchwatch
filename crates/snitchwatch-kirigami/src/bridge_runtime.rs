//! In-process bridge runtime for the native Kirigami shell (Task 13).
//!
//! Ported from `snitchwatch-tauri`'s `bridge_runtime.rs`. The job is the same:
//! spawn `snitchwatch_bridge_cli::run` on a background multi-thread Tokio
//! runtime and hold its handles for the process lifetime. Two things differ
//! from the Tauri port, both because the native shell links the bridge as a
//! *library* and consumes its typed channels directly (per the plan's
//! architecture note):
//!
//!   * **No loopback proxy.** The Tauri shell round-tripped the WS Unix socket
//!     back to a loopback TCP port for its WebKitGTK webview. The native shell
//!     never speaks WS to itself — it subscribes to
//!     [`BridgeHandles::subscribe`] and pushes to [`BridgeHandles::inbound_tx`]
//!     directly — so that whole proxy is gone.
//!   * **A `OnceLock` singleton.** The QML-instantiated models each need the
//!     bridge handles from their own `startBridgeFeed()` init path, and there is
//!     no Rust-owned parent to thread them through (QML owns the object tree).
//!     [`ensure_started`] initializes the singleton exactly once from `main`;
//!     [`handles`] / [`status`] are pure reads that never start anything, so
//!     tests that load QML without calling [`ensure_started`] bind no sockets.
//!
//! Startup failure (gRPC port already bound, no `$XDG_RUNTIME_DIR`, …) is
//! captured, not panicked: the window still opens and [`status`] surfaces the
//! error text for a `Kirigami.InlineMessage`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_bridge_cli::{BridgeConfig, RunningBridge};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{broadcast, mpsc, watch};

/// Cheaply-clonable handles into the running in-process bridge, handed to each
/// model's live feed. Cloning is cheap: two channel senders and a runtime
/// handle (all `Arc`-backed internally).
#[derive(Clone)]
pub struct BridgeHandles {
    broadcast_tx: broadcast::Sender<ServerMessage>,
    inbound_tx: mpsc::Sender<ClientMessage>,
    runtime: Handle,
}

impl BridgeHandles {
    /// Subscribe to the outbound `ServerMessage` stream. Each model's feed task
    /// gets its own receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.broadcast_tx.subscribe()
    }

    /// A clone of the inbound sender, for pushing UI-origin `ClientMessage`s
    /// onto the bridge's upstream pump.
    pub fn inbound_tx(&self) -> mpsc::Sender<ClientMessage> {
        self.inbound_tx.clone()
    }

    /// The background runtime's handle, so feed tasks spawn onto the same
    /// runtime the bridge itself runs on (never the Qt/GUI thread).
    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }
}

/// The runtime + running bridge, kept alive for the process lifetime. Dropping
/// either is unsafe-by-consequence: dropping the `Runtime` aborts every bridge
/// task, and dropping `RunningBridge` drops its shutdown `oneshot::Sender`s,
/// which the bridge's `select!` loops observe as a shutdown signal.
///
/// Held behind a `Mutex` purely to make the enclosing singleton `Sync`:
/// `RunningBridge` retains tokio receivers whose `Sync`-ness isn't guaranteed,
/// but it is `Send`, so `Mutex<Kept>: Sync`. Nothing ever locks it on a hot
/// path — the cheap, `Sync` accessors below (`handles`, `grpc_addr`) are stored
/// outside it.
struct Kept {
    _runtime: Runtime,
    _bridge: RunningBridge,
}

/// The running state exposed to the shell. Everything read on a hot path is a
/// cheap `Sync` value stored directly; `_kept` only holds the world alive.
struct RunningBridgeRuntime {
    handles: BridgeHandles,
    grpc_addr: SocketAddr,
    _kept: Mutex<Kept>,
}

/// Outcome of the one-shot start. `Failed` carries UI-surfacable error text.
/// The running variant is boxed so the two variants stay a similar size.
enum Outcome {
    Running(Box<RunningBridgeRuntime>),
    Failed(String),
}

static STARTED: OnceLock<Outcome> = OnceLock::new();

/// Build the bridge config, mirroring `snitchwatch-tauri`'s defaults so the
/// native shell is a drop-in: a fixed gRPC port `opensnitchd` dials, and the WS
/// Unix socket under the XDG runtime dir. Both are overridable via the same env
/// vars `BridgeConfig::from_env` honors, so existing deployments keep working.
fn bridge_config() -> anyhow::Result<BridgeConfig> {
    let grpc_bind = std::env::var("SNITCHWATCH_GRPC_BIND")
        .unwrap_or_else(|_| "127.0.0.1:50051".to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid SNITCHWATCH_GRPC_BIND: {e}"))?;
    let ws_socket_path = std::env::var_os("SNITCHWATCH_WS_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| snitchwatch_bridge::auth::runtime_dir().join("bridge.sock"));
    Ok(BridgeConfig {
        grpc_bind,
        ws_socket_path,
        cache_capacity: 10_000,
    })
}

fn start_inner() -> anyhow::Result<RunningBridgeRuntime> {
    // A multi-thread runtime, exactly as the Tauri shell built one (both host a
    // non-Tokio event loop alongside, so `#[tokio::main]` isn't an option).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let cfg = bridge_config()?;
    let bridge = runtime.block_on(async { snitchwatch_bridge_cli::run(cfg).await })?;
    let handles = BridgeHandles {
        broadcast_tx: bridge.broadcast_tx.clone(),
        inbound_tx: bridge.inbound_tx.clone(),
        runtime: runtime.handle().clone(),
    };
    let grpc_addr = bridge.grpc_addr;
    Ok(RunningBridgeRuntime {
        handles,
        grpc_addr,
        _kept: Mutex::new(Kept {
            _runtime: runtime,
            _bridge: bridge,
        }),
    })
}

/// Start the in-process bridge exactly once. Idempotent and thread-safe: the
/// first caller runs startup, later callers observe the same outcome. Returns
/// `(ok, message)` where `message` is a human-readable status line for logging
/// and for the app-level status surface. Call this once from `main` *before*
/// loading QML so startup is deterministic and off the Qt thread.
pub fn ensure_started() -> (bool, String) {
    let outcome = STARTED.get_or_init(|| match start_inner() {
        Ok(rt) => Outcome::Running(Box::new(rt)),
        Err(e) => {
            tracing::error!(error = %e, "in-process bridge failed to start");
            Outcome::Failed(format!("{e:#}"))
        }
    });
    status_of(outcome)
}

/// The current handles, or `None` if the bridge was never started (tests that
/// load QML without [`ensure_started`]) or failed to start. This is a pure read
/// — it never triggers startup, so calling it from a model's `startBridgeFeed`
/// in a headless test binds no sockets and simply no-ops the live feed.
pub fn handles() -> Option<BridgeHandles> {
    match STARTED.get()? {
        Outcome::Running(rt) => Some(rt.handles.clone()),
        Outcome::Failed(_) => None,
    }
}

/// Current bridge status for the UI, or `None` if [`ensure_started`] was never
/// called. Pure read; never triggers startup.
pub fn status() -> Option<(bool, String)> {
    STARTED.get().map(status_of)
}

/// A clone of the bridge's tray-state watch receiver (Task 18's
/// `TrayController` feed), or `None` if the bridge never started/failed to
/// start. Pure read; never triggers startup — a headless test that never
/// calls [`ensure_started`] gets `None` and simply no-ops its live feed.
pub fn tray_rx() -> Option<watch::Receiver<BridgeTrayState>> {
    match STARTED.get()? {
        Outcome::Running(rt) => Some(
            rt._kept
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                ._bridge
                .tray_rx
                .clone(),
        ),
        Outcome::Failed(_) => None,
    }
}

/// A fresh subscription to the bridge's notice broadcast (Task 17's
/// `NotificationController` feed), or `None` under the same conditions as
/// [`tray_rx`]. `resubscribe()` (not `.clone()`) because
/// `broadcast::Receiver` cursors are per-subscriber; each caller needs its own
/// starting from "now", mirroring `BridgeHandles::subscribe`.
pub fn notice_rx() -> Option<broadcast::Receiver<BridgeNotice>> {
    match STARTED.get()? {
        Outcome::Running(rt) => Some(
            rt._kept
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                ._bridge
                .notice_rx
                .resubscribe(),
        ),
        Outcome::Failed(_) => None,
    }
}

fn status_of(outcome: &Outcome) -> (bool, String) {
    match outcome {
        Outcome::Running(rt) => (true, format!("Connected to bridge (gRPC {})", rt.grpc_addr)),
        Outcome::Failed(msg) => (false, format!("Bridge unavailable: {msg}")),
    }
}

// `Notice` / `TrayState` are re-exported so the shell-chrome tasks that
// consume `tray_rx()` / `notice_rx()` (`TrayController`, Task 18;
// `NotificationController`, Task 17) have a single import site.
pub use snitchwatch_bridge::notice::Notice as BridgeNotice;
pub use snitchwatch_bridge::tray_state::TrayState as BridgeTrayState;
