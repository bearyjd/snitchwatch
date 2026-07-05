//! Typed errors for the privileged-tier scanner.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("store mutex poisoned")]
    Poisoned,

    #[error("i/o error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A required system primitive is not available on this host (command
    /// not found, sysfs path absent). Callers downgrade this to a
    /// `SkippedCheck` note rather than failing the whole scan — a degraded
    /// scan should still record whatever cheaper checks *could* run.
    #[error("primitive unavailable: {0}")]
    PrimitiveUnavailable(String),
}

pub type Result<T> = std::result::Result<T, ScanError>;
