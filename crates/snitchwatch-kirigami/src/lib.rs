//! Snitchwatch Kirigami shell — library target.
//!
//! The binary (`main.rs`) is a thin wrapper that boots `QGuiApplication` and
//! loads the QML entry point. Everything testable lives here so integration
//! tests under `tests/` link against the same cxx-qt generated code (the
//! Phase 3a spike proved cxx-qt issue #770 does not bite this repo — see
//! `crates/kirigami-spike/tests/link_770.rs`).

pub mod bridge_bindings;
