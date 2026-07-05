//! Snitchwatch Kirigami shell — library target.
//!
//! The binary (`main.rs`) is a thin wrapper that boots `QGuiApplication` and
//! loads the QML entry point. Everything testable lives here so integration
//! tests under `tests/` link against the same cxx-qt generated code (the
//! Phase 3a spike proved cxx-qt issue #770 does not bite this repo — see
//! `crates/kirigami-spike/tests/link_770.rs`).

pub mod autostart;
pub mod blocklists;
pub mod blocklists_model;
pub mod bridge_bindings;
pub mod bridge_dispatch;
pub mod bridge_feed;
pub mod bridge_runtime;
pub mod connections;
pub mod connections_model;
pub mod crash_log;
pub mod geo;
pub mod geo_model;
pub mod notification_controller;
pub mod notifier;
pub mod panic_hook;
pub mod paths;
pub mod pending_decision;
pub mod profiles;
pub mod profiles_model;
pub mod rules;
pub mod rules_model;
pub mod settings_controller;
pub mod traffic;
pub mod traffic_model;
pub mod tray;
pub mod tray_controller;
pub mod wizard;
pub mod wizard_controller;
