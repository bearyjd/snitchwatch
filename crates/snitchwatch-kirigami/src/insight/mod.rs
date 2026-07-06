//! Research/insight side-channel for the pending-decision dialog (Parity 2).
//!
//! For the remote IP of a pending connection, this fetches:
//!   * a reverse-DNS (PTR) hostname, and
//!   * RDAP registration info (organization / registrar / country).
//!
//! Both lookups run on the bridge's Tokio runtime with a hard 4-second
//! timeout each ([`client::LOOKUP_TIMEOUT`]) and are cached per-IP for the
//! session (see [`model`]'s `PendingInsight::lookup`).
//!
//! **Strictly decorative — never on the safety-critical path.** A lookup
//! failure, timeout, or total absence of network must never delay or block
//! submitting a verdict: the QML dialog only *displays* whatever this reports
//! (or "unavailable (offline?)"); it never gates the Allow/Deny buttons, and
//! nothing here is awaited synchronously by any qinvokable.
//!
//! The pure fetch/cache/parse logic lives in [`client`] and is unit-tested
//! against a fake [`client::InsightSource`] — no real network I/O in tests.
//! The thin cxx-qt `PendingInsight` QObject wrapper lives in
//! `crate::insight_model` (a sibling of `src/`, not a submodule here — see
//! that file's module docs for why), following the same
//! async-dispatch-then-`qt_thread.queue` pattern as `TrafficModel` and
//! `BlocklistsModel`.

pub mod client;

pub use client::{InsightResult, InsightSource, RdapInfo, RealInsightSource};
