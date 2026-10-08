//! Snitchwatch bridge — translates between Little Snitch's WebSocket protocol
//! and OpenSnitch's gRPC protocol.
//!
//! This crate is intentionally headless. It can be exercised against either a
//! real opensnitchd (via the gRPC client) or `tests/mock_opensnitchd` (an
//! in-process tonic server). It does not depend on Tauri, WebKitGTK, or any
//! windowing system.

pub mod auth;
pub mod blocklists;
pub mod bridge_capabilities;
pub mod cache;
pub mod client_presence;
pub mod curated;
pub mod daemon_alerts;
pub mod daemon_commands;
pub mod daemon_config;
pub mod daemon_contract;
pub mod daemon_liveness;
pub mod daemon_watchdog;
pub mod deferred_answers;
pub mod diagnostics;
pub mod error;
pub mod filter_pause;
pub mod grpc_client;
pub mod grpc_server;
pub mod notice;
pub mod pause_answers;
pub mod profiles;
pub mod prompt_slot;
pub mod rule_io;
pub mod rule_name;
pub mod rule_policy;
pub mod rule_wire;
pub mod sqlite_file;
pub(crate) mod state_file;
pub mod translator;
pub mod tray_state;
#[cfg(feature = "web-ui")]
pub mod web_assets;
pub mod ws_messages;
pub mod ws_server;

pub use error::BridgeError;
