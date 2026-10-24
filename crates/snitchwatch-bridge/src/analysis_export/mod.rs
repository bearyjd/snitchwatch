//! Bounded, private source projection for cache-only analysis exports.

mod build;
mod dto;
mod redaction;
mod snapshot;

pub use build::{build, ANALYSIS_PROMPT};
pub use redaction::fresh_key;

#[cfg(test)]
mod redaction_tests;

#[cfg(test)]
mod build_tests;

pub use dto::*;
pub use snapshot::{capture, Snapshot};

#[cfg(test)]
mod snapshot_tests;
