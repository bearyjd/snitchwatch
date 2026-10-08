//! Rules domain: the Rules tab's data model (Task 10).
//!
//! - [`row_store`]: pure, Qt-free store for the flat rule list — fully
//!   unit-tested here.
//! - [`all_apps`]: flags pre-#50 Snitchwatch prompt rules that match every
//!   program (issue #44, second half).
//! - [`simulator`]: pure, Qt-free rule-match simulator (Little-Snitch-parity
//!   "rule-match diagnostics" simulate panel) — evaluates a candidate
//!   connection against `row_store`'s cached rules the way opensnitchd
//!   v1.8.0 does: every operand it matches on, its rule order, and its
//!   comparison semantics. Inputs left blank are reported as not evaluated.
//! - The cxx-qt `QAbstractListModel` wrapper that binds this to QML lives in
//!   the top-level [`crate::rules_model`] module (kept flat under `src/`
//!   with the other `#[cxx_qt::bridge]` files, per the same cxx-qt-build
//!   one-directory constraint noted in [`crate::connections`]).

pub mod all_apps;
pub mod row_store;
pub mod simulator;
