//! Blocklists domain: the Blocklists tab's data model (Task 9).
//!
//! - [`row_store`]: pure, Qt-free stores for the two-level blocklists view —
//!   the subscription list and the per-subscription entry list — fully
//!   unit-tested here.
//! - The cxx-qt `QAbstractListModel` wrappers that bind these to QML live in
//!   the top-level [`crate::blocklists_model`] module (kept flat under `src/`
//!   with the other `#[cxx_qt::bridge]` files, per the same cxx-qt-build
//!   one-directory constraint noted in [`crate::connections`]).

pub mod row_store;
