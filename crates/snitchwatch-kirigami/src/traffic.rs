//! Traffic domain: the Traffic tab's data model (Task 11).
//!
//! - [`ring_store`]: a pure, Qt-free fixed-window store — see its module docs
//!   for the spike verdict that led to a QML `Canvas` chart instead of
//!   `QtCharts`/`QtGraphs`.
//! - The cxx-qt `QObject` wrapper that binds this to QML lives in the
//!   top-level [`crate::traffic_model`] module (kept flat under `src/` with
//!   the other `#[cxx_qt::bridge]` files, per the same cxx-qt-build
//!   one-directory constraint noted in [`crate::connections`]).

pub mod ring_store;
