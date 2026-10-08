//! Rules domain: the Rules tab's data model (Task 10).
//!
//! - [`row_store`]: pure, Qt-free store for the flat rule list — fully
//!   unit-tested here.
//! - [`all_apps`]: flags pre-#50 Snitchwatch prompt rules that match every
//!   program (issue #44, second half).
//! - [`io`] and [`io_view`]: rule import/export (roadmap P2.7) without Qt:
//!   the bounded file read, the owner-only export write, and the preview
//!   rows and texts. `crate::rules_io_controller` binds them to QML.
//! - [`editor`] and [`editor_view`]: the rule editor (roadmap P2.1) without
//!   Qt: the condition builder, the wire shape, the bridge's own checks, and
//!   what is sent and said.
//!   `crate::rule_editor_controller` binds it to QML.
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
pub mod editor;
pub mod editor_profile;
pub mod editor_view;
pub mod hits;
pub mod io;
pub mod io_view;
pub mod row_store;
pub mod simulator;

#[cfg(test)]
mod editor_tests;

#[cfg(test)]
mod editor_view_tests;

#[cfg(test)]
mod io_tests;

#[cfg(test)]
mod io_view_tests;
