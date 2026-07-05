//! Snitchwatch desktop shell library surface.
//!
//! All real work lives in the modules below; `main.rs` is just a thin
//! orchestrator. Splitting the work into a library makes the modules
//! independently testable with `cargo test -p snitchwatch-tauri`.

pub mod bridge_runtime;
pub mod commands;
pub mod loopback_proxy;
pub mod notifier;
pub mod panic_hook;
pub mod paths;
pub mod tray;
pub mod wizard;
