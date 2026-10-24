//! Immutable cache-summary preview state and explicit, private local saving.
mod save;
mod view;

pub use save::{save_artifact, Durability, SaveError, SavedArtifact};
pub use view::ExportView;
