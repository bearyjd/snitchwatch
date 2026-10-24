//! Bounded, private source projection for cache-only analysis exports.

mod dto;
mod snapshot;

pub use dto::*;
pub use snapshot::{capture, Snapshot};

#[cfg(test)]
mod snapshot_tests;
