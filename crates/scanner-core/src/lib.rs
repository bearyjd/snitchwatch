//! Component B — Bazzite security scanner, userspace tier.
//!
//! This crate is the **orchestration + classification + diffing** layer for
//! the scanner's userspace-default tier, implementing
//! `docs/superpowers/specs/2026-07-04-scanner-baseline-design.md`.
//!
//! Design principle (baseline design §"Candidate approach evaluation"):
//! *don't reimplement what `ostree`/`rpm-ostree`/`rpm` already know.* Every
//! shell-out to those tools sits behind the [`inspector::SystemInspector`]
//! trait so the hard logic — the five-step expected-drift-vs-anomaly
//! classification tree ([`classify`]) and the new/still-outstanding/resolved
//! findings diff ([`scans`]) — is pure and unit-testable without a real
//! rpm-ostree/Bazzite host.
//!
//! ## Tiers
//!
//! Only the **userspace tier** lives here (Phase 5). Per baseline design §2
//! it deliberately does *not* perform a full AIDE-style content-hash pass —
//! that expensive, privilege-justifying work belongs to the privileged tier
//! (Phase 6). The userspace tier leans on cheap metadata and per-surface
//! baseline diffs it can build without elevation.
//!
//! ## Deferred
//!
//! The "unusual outbound connections" check ([`checks::outbound`]) is a
//! deliberate no-op in this release: it depends on a persisted
//! connection-log signal channel from Component A that does not exist yet
//! (see `IMPLEMENTATION_PROMPT.md` Phase 5 precondition). It is wired in as
//! a deferred stub, not silently omitted.

pub mod checks;
pub mod classify;
pub mod deployment;
pub mod finding;
pub mod inspector;
pub mod provenance;
pub mod rpmverify;
pub mod scanner;
pub mod stores;
pub mod testkit;

pub use classify::{classify, Classification, LayeredMatch, PathFacts};
pub use deployment::DeploymentState;
pub use finding::{Anomaly, FindingStatus, ReportedFinding, ScanReport};
pub use inspector::{CommandOutput, InspectError, RealSystem, SystemInspector};
pub use scanner::{Scanner, ScannerError};
