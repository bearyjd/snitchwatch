//! Ordered privileged-tier sub-checks. Each module is pure over its inputs
//! (or reads the host only through [`crate::facts::SystemFacts`]) so it is
//! testable with synthetic fixtures.

pub mod kargs;
pub mod lockdown;
pub mod modules;
pub mod rootkit;
