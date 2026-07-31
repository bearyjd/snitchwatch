//! Snitchwatch bridge — translates between Little Snitch's WebSocket protocol
//! and OpenSnitch's gRPC protocol.
//!
//! This crate is intentionally headless. It can be exercised against either a
//! real opensnitchd (via the gRPC client) or `tests/mock_opensnitchd` (an
//! in-process tonic server). It does not depend on Tauri, WebKitGTK, or any
//! windowing system.

pub mod auth;
pub mod blocklists;
pub mod cache;
pub mod daemon_alerts;
pub mod daemon_liveness;
pub mod daemon_watchdog;
pub mod diagnostics;
pub mod error;
pub mod grpc_client;
pub mod grpc_server;
pub mod notice;
pub mod profiles;
pub mod translator;
pub mod tray_state;
pub mod web_assets;
pub mod ws_messages;
pub mod ws_server;

pub use error::BridgeError;
