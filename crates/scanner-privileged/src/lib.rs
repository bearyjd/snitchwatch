//! Component B — privileged-tier security scanner (library).
//!
//! This crate implements the on-demand, polkit/pkexec-invoked privileged
//! scanner specified in
//! `docs/superpowers/specs/2026-07-04-scanner-privileged-tier-design.md`.
//!
//! # Design invariants (do not regress)
//!
//! - **Never a daemon.** This is a transient binary. There is no server,
//!   socket listener, background thread, timer, or systemd service anywhere
//!   in this crate. Every host read is a one-shot. Component B's whole design
//!   exists to avoid the "always-on privileged daemon"; reintroducing any
//!   long-running privileged process here is a real regression, not a nit.
//! - **Delegate, don't reimplement.** Detection primitives (`chkrootkit`,
//!   `rpm-ostree`, `rpm -V`, `modinfo`, sysfs) are wrapped, not reinvented.
//! - **Shared schema.** Findings feed the same `scans.db` schema the baseline
//!   design doc specifies, extended with a `check_type` column — see
//!   [`store`] and its reconciliation note for the Phase 5 merge plan.
//!
//! # Testability
//!
//! All host reads go through the [`facts::SystemFacts`] trait, so the
//! classification/diff logic is testable with synthetic fixtures — no real
//! hardware, `rpm-ostree`, or `chkrootkit` required. [`facts::LiveSystem`] is
//! the production implementation that shells out to the real primitives.

pub mod checks;
pub mod error;
pub mod facts;
pub mod model;
pub mod scan;
pub mod store;

pub use error::{Result, ScanError};
pub use facts::{LiveSystem, SystemFacts};
pub use model::{CheckType, Classification, Finding, SkippedCheck};
pub use scan::{run_privileged_scan, ScanReport};
pub use store::{DiffStatus, ReconcileReport, ScanStore};

/// polkit action id gating the deep-scan invocation. Must match the
/// `<action id>` in `packaging/polkit/org.snitchwatch.scanner.policy`.
pub const POLKIT_ACTION_ID: &str = "org.snitchwatch.scanner.run-deep-scan";
