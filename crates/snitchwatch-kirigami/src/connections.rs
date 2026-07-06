//! Connection-list domain: the core safety-critical loop's data model (Task 6).
//!
//! - [`row_store`]: pure, Qt-free ordering/CRUD logic (fully unit-tested here).
//! - The cxx-qt `QAbstractListModel` wrapper that binds this to QML lives in
//!   the top-level [`crate::connections_model`] module. It is kept flat under
//!   `src/` (not here under `src/connections/`) because cxx-qt-build requires
//!   all of a QML module's `#[cxx_qt::bridge]` `rust_files` to share one
//!   directory (Qt bug QTBUG-93443), alongside `bridge_bindings.rs`.

pub mod filter;
pub mod grouping;
pub mod row_store;
