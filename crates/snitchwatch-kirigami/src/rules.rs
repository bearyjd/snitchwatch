//! Rules domain: the Rules tab's data model (Task 10).
//!
//! - [`row_store`]: pure, Qt-free store for the flat rule list — fully
//!   unit-tested here.
//! - [`simulator`]: pure, Qt-free rule-match simulator (Little-Snitch-parity
//!   "rule-match diagnostics" simulate panel) — evaluates a candidate
//!   process/host/port/protocol against `row_store`'s cached rules using
//!   opensnitchd's own precedence semantics.
//! - The cxx-qt `QAbstractListModel` wrapper that binds this to QML lives in
//!   the top-level [`crate::rules_model`] module (kept flat under `src/`
//!   with the other `#[cxx_qt::bridge]` files, per the same cxx-qt-build
//!   one-directory constraint noted in [`crate::connections`]).

pub mod row_store;
pub mod simulator;
